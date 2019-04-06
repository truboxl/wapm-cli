use crate::dependency_resolver::{Dependency, DependencyResolver};
use crate::lock::lockfile_command::LockfileCommand;
use crate::lock::lockfile_module::LockfileModule;
use crate::lock::{LOCKFILE_HEADER, LOCKFILE_NAME};
use crate::manifest::{extract_dependencies, Manifest};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;

#[derive(Debug, Deserialize, Serialize, PartialEq)]
pub struct Lockfile {
    pub modules: BTreeMap<String, LockfileModule>,
    pub commands: BTreeMap<String, LockfileCommand>,
}

impl Lockfile {
    pub fn open<P: AsRef<Path>>(directory: P) -> Result<Self, failure::Error> {
        let lockfile_path = directory.as_ref().join(LOCKFILE_NAME);
        let mut lockfile_file = File::open(lockfile_path)?;
        let mut lockfile_string = String::new();
        lockfile_file.read_to_string(&mut lockfile_string)?;
        let lockfile: Lockfile = toml::from_str(&lockfile_string)?;
        Ok(lockfile)
    }

    /// This method constructs a new lockfile with just a manifest. This is typical if no lockfile
    /// previously exists. All dependencies will be fetched.
    pub fn new_from_manifest<D: DependencyResolver>(
        manifest: &Manifest,
        dependency_resolver: &D,
    ) -> Result<Self, failure::Error> {
        let mut lockfile_modules = BTreeMap::new();
        let mut lockfile_commands = BTreeMap::new();
        let dependencies = match manifest.dependencies {
            Some(ref dependencies) => extract_dependencies(dependencies)?,
            None => vec![],
        };
        let mut manifests = vec![];
        for (name, version) in dependencies.iter() {
            let dependency_manifest = dependency_resolver.resolve(name, version)?;
            manifests.push(dependency_manifest);
        }
        for manifest in manifests.iter() {
            get_lockfile_data_from_manifest(
                &manifest,
                &mut lockfile_modules,
                &mut lockfile_commands,
            );
        }
        // handle this manifest's commands
        get_commands_from_manifest(&manifest, &mut lockfile_commands);

        let new_lockfile = Lockfile {
            modules: lockfile_modules,
            commands: lockfile_commands,
        };

        Ok(new_lockfile)
    }

    /// This function takes a manifest, maybe a lockfile, and a dependency resolver. The output is
    /// a new lockfile that resolves changes that have been made to the manifest file and the
    /// existing lockfile, if it exists. The resolver is used to fetch the manifest for packages
    /// that are new i.e. packages that have been added to the manifest and not been updated in
    /// the lockfile.
    pub fn new_from_manifest_and_lockfile<D: DependencyResolver>(
        manifest: &Manifest,
        existing_lockfile: Lockfile,
        dependency_resolver: &D,
    ) -> Result<Self, failure::Error> {
        // capture references to the parts of the lockfile
        let existing_lockfile_module = &existing_lockfile.modules;
        let existing_lockfile_commands = &existing_lockfile.commands;
        // get all dependencies that changed and references to unchanged lockfile modules
        let (changed_dependencies, unchanged_lockfile_modules) =
            resolve_changes(&manifest, existing_lockfile_module)?;
        // get all (unchanged) commands for unchanged lockfile modules
        let mut lockfile_commands = BTreeMap::new();
        for (key, _lockfile_module) in unchanged_lockfile_modules.iter() {
            for (name, command) in existing_lockfile_commands
                .iter()
                .filter(|(_name, c)| &c.module == key)
            {
                lockfile_commands.insert(name.clone(), command.clone());
            }
        }
        // copy all lockfile modules into a map
        let mut lockfile_modules = unchanged_lockfile_modules;
        // for all changed dependencies, fetch the newest manifest
        for (name, version) in changed_dependencies {
            let dependency_manifest = dependency_resolver.resolve(&name, &version)?;
            get_lockfile_data_from_manifest(
                &dependency_manifest,
                &mut lockfile_modules,
                &mut lockfile_commands,
            );
        }

        // handle this manifest's commands
        get_commands_from_manifest(&manifest, &mut lockfile_commands);

        let new_lockfile = Lockfile {
            modules: lockfile_modules,
            commands: lockfile_commands,
        };

        Ok(new_lockfile)
    }

    /// Save the lockfile to the directory.
    pub fn save<P: AsRef<Path>>(&self, directory: P) -> Result<(), failure::Error> {
        let lockfile_string = toml::to_string(self)?;
        let lockfile_string = format!("{}\n{}", LOCKFILE_HEADER, lockfile_string);
        let lockfile_path = directory.as_ref().join(LOCKFILE_NAME);
        let mut file = File::create(&lockfile_path)?;
        file.write_all(lockfile_string.as_bytes())?;
        Ok(())
    }

    pub fn get_command(&self, command_name: &str) -> Result<&LockfileCommand, failure::Error> {
        self.commands
            .get(command_name)
            .ok_or(LockfileError::CommandNotFound(command_name.to_string()).into())
    }

    pub fn get_module(&self, module_name: &str) -> Result<&LockfileModule, failure::Error> {
        self.modules
            .get(module_name)
            .ok_or(LockfileError::ModuleNotFound(module_name.to_string()).into())
    }
}

#[derive(Debug, Fail)]
pub enum LockfileError {
    #[fail(display = "Command not found: {}", _0)]
    CommandNotFound(String),
    #[fail(display = "Module not found: {}", _0)]
    ModuleNotFound(String),
}

/// This helper function resolves differences between the lockfile and the manifest file. All changes
/// that have not been reflected in the lockfile are returned as a vec of package names and versions.
/// The packages that had no changes are returned as references to the the lockfile modules.
fn resolve_changes<'b>(
    manifest: &'b Manifest,
    lockfile_modules: &BTreeMap<String, LockfileModule>,
) -> Result<(Vec<(&'b str, &'b str)>, BTreeMap<String, LockfileModule>), failure::Error> {
    let (changes, not_changed) = match manifest.dependencies {
        Some(ref dependencies) => {
            let mut changes = vec![];
            let mut not_changed = BTreeMap::new();
            let dependencies = extract_dependencies(dependencies)?;
            for (name, version) in dependencies.iter() {
                let key = format!("{} {}", name, version);
                match lockfile_modules.get(&key) {
                    Some(lockfile_module) => {
                        not_changed.insert(key, lockfile_module.clone());
                    }
                    None => changes.push((*name, *version)),
                }
            }
            (changes, not_changed)
        }
        None => (vec![], BTreeMap::new()),
    };
    Ok((changes, not_changed))
}

fn get_lockfile_data_from_manifest(
    dependency: &Dependency,
    lockfile_modules: &mut BTreeMap<String, LockfileModule>,
    lockfile_commands: &mut BTreeMap<String, LockfileCommand>,
) {
    let manifest = &dependency.manifest;
    let download_url = dependency.download_url.as_str();
    match manifest.module {
        Some(ref module) => {
            let name = &dependency.name;
            let lockfile_module = LockfileModule::from_module(name.to_string(), module, download_url);
            let key = format!(
                "{} {}",
                lockfile_module.name.clone(),
                lockfile_module.version.clone()
            );
            lockfile_modules.insert(key.clone(), lockfile_module);
            // if there is a module, then get the commands if any exist
            match manifest.command {
                Some(ref commands) => {
                    for command in commands {
                        let lockfile_command = LockfileCommand::from_command(&key, command);
                        lockfile_commands.insert(command.name.clone(), lockfile_command);
                    }
                }
                None => {}
            }
        }
        None => {}
    }
}

fn get_commands_from_manifest(
    manifest: &Manifest,
    lockfile_commands: &mut BTreeMap<String, LockfileCommand>,
) {
    // handle this manifest's commands
    match (&manifest.command, &manifest.module) {
        (Some(commands), Some(module)) => {
            for command in commands {
                let module_string = format!("{} {}", module.name, module.version);
                let lockfile_command = LockfileCommand::from_command(&module_string, command);
                lockfile_commands.insert(command.name.clone(), lockfile_command);
            }
        }
        (_, _) => {} // if there is no module, then there are no commands
    };
}

#[cfg(test)]
mod get_command_tests {
    use crate::lock::Lockfile;

    #[test]
    fn get_commands() {
        let wapm_lock_toml = toml! {
            [modules."foo 1.0.0"]
            name = "foo"
            version = "1.0.0"
            source = ""
            resolved = ""
            integrity = ""
            hash = ""
            abi = "None"
            entry = "target.wasm"
            [modules."bar 3.0.0"]
            name = "bar"
            version = "3.0.0"
            source = ""
            resolved = ""
            integrity = ""
            hash = ""
            abi = "None"
            entry = "target.wasm"
            [commands.bar]
            module = "bar 3.0.0"

        };
        let lockfile: Lockfile = wapm_lock_toml.try_into().unwrap();

        let foo_command_name = "foo";
        let bar_command_name = "bar";

        let result = lockfile.get_command(foo_command_name);
        assert!(result.is_err());

        let result = lockfile.get_command(bar_command_name);
        assert!(result.is_ok());
    }
}

#[cfg(test)]
mod get_lockfile_data_from_manifest_tests {
    use crate::dependency_resolver::Dependency;
    use crate::lock::lockfile::get_lockfile_data_from_manifest;
    use crate::manifest::Manifest;
    use std::collections::BTreeMap;

    #[test]
    fn fill_lockfile_data() {
        let mut lockfile_modules = BTreeMap::new();
        let mut lockfile_commands = BTreeMap::new();
        let foo_toml: toml::Value = toml! {
            [module]
            name = "foo"
            version = "1.0.0"
            module = "foo.wasm"
            description = ""
            [[command]]
            name = "do_foo_stuff"
            [[command]]
            name = "do_other_stuff"
        };
        let foo_manifest: Manifest = foo_toml.try_into().unwrap();
        let dependency = Dependency {
            name: "foo".to_string(),
            manifest: foo_manifest,
            download_url: "".to_string(),
        };
        get_lockfile_data_from_manifest(&dependency, &mut lockfile_modules, &mut lockfile_commands);
        assert_eq!(1, lockfile_modules.len());
        assert_eq!(2, lockfile_commands.len());
    }
}

#[cfg(test)]
mod resolve_changes_tests {
    use crate::lock::lockfile::{resolve_changes, Lockfile};
    use crate::manifest::Manifest;

    #[test]
    fn lock_file_exists_and_one_unchanged_dependency() {
        let wapm_toml = toml! {
            [module]
            name = "test"
            version = "1.0.0"
            module = "target.wasm"
            description = "description"
            [dependencies]
            foo = "1.0.0"
            bar = "2.0.1"
        };
        let manifest: Manifest = wapm_toml.try_into().unwrap();
        let wapm_lock_toml = toml! {
            [modules."foo 1.0.0"]
            name = "foo"
            version = "1.0.0"
            source = ""
            resolved = ""
            integrity = ""
            hash = ""
            abi = "None"
            entry = "target.wasm"
            [modules."bar 3.0.0"]
            name = "bar"
            version = "3.0.0" // THIS CHANGED!
            source = ""
            resolved = ""
            integrity = ""
            hash = ""
            abi = "None"
            entry = "target.wasm"
            [commands]
        };
        let lockfile: Lockfile = wapm_lock_toml.try_into().unwrap();
        let lockfile_modules = lockfile.modules;
        let (changes, not_changed) = resolve_changes(&manifest, &lockfile_modules).unwrap();
        assert_eq!(1, changes.len()); // one dependency was upgraded
        assert_eq!(1, not_changed.len()); // one dependency did not change, reuse the lockfile module
    }
}

#[cfg(test)]
mod test {
    use crate::dependency_resolver::{Dependency, TestResolver};
    use crate::lock::lockfile::Lockfile;
    use crate::lock::LOCKFILE_NAME;
    use crate::manifest::{Manifest, MANIFEST_FILE_NAME};
    use std::collections::BTreeMap;
    use std::fs::File;
    use std::io::Write;

    #[test]
    fn open_nonexistent_lockfile() {
        let tmp_dir = tempdir::TempDir::new("open_nonexistent_lockfile").unwrap();
        let lockfile_result = Lockfile::open(tmp_dir.as_ref());
        assert!(lockfile_result.is_err());
    }
    #[test]
    fn open_lockfile() {
        let tmp_dir = tempdir::TempDir::new("open_lockfile").unwrap();
        let wapm_lock_toml = toml! {
            [modules."test 1.0.0"]
            name = "test"
            version = "1.0.0"
            source = ""
            resolved = ""
            integrity = ""
            hash = ""
            abi = "None"
            entry = "source.wasm"

            [commands.foo]
            module = "foo@0.1.1"
        };
        let lock_path = tmp_dir.path().join(LOCKFILE_NAME);
        let mut file = File::create(&lock_path).unwrap();
        let toml_string = toml::to_string(&wapm_lock_toml).unwrap();
        file.write_all(toml_string.as_bytes()).unwrap();
        let _lockfile_result = Lockfile::open(tmp_dir.as_ref()).unwrap();
    }
    #[test]
    fn create_from_manifest() {
        let tmp_dir = tempdir::TempDir::new("create_from_manifest").unwrap();
        let wapm_toml = toml! {
            [module]
            name = "test"
            version = "1.0.0"
            module = "target.wasm"
            description = "description"
        };
        let manifest_path = tmp_dir.path().join(MANIFEST_FILE_NAME);
        let mut file = File::create(&manifest_path).unwrap();
        let toml_string = toml::to_string(&wapm_toml).unwrap();
        file.write_all(toml_string.as_bytes()).unwrap();
        let manifest = Manifest::open(manifest_path).unwrap();

        let resolver = TestResolver(BTreeMap::new());

        let lockfile = Lockfile::new_from_manifest(&manifest, &resolver).unwrap();
        assert_eq!(0, lockfile.commands.len());
        assert_eq!(0, lockfile.modules.len());
    }
    #[test]
    fn create_from_manifest_and_existing_lockfile_with_dependencies_and_commands() {
        let wapm_toml = toml! {
            [module]
            name = "test"
            version = "1.0.0"
            module = "target.wasm"
            description = "description"
            [dependencies]
            foo = "1.0.2"
            bar = "3.0.0"
        };
        let manifest: Manifest = wapm_toml.try_into().unwrap();

        // setup resolver
        let mut map = BTreeMap::new();
        // FOO package v 1.0.0
        let foo_toml: toml::Value = toml! {
            [module]
            name = "foo"
            version = "1.0.0"
            module = "foo.wasm"
            description = ""
            [[command]]
            name = "do_foo_stuff"
        };
        let foo_manifest: Manifest = foo_toml.try_into().unwrap();
        let foo_dependency = Dependency {
            name: "foo".to_string(),
            manifest: foo_manifest,
            download_url: "".to_string(),
        };
        // FOO package v 1.0.2
        map.insert(("foo".to_string(), "1.0.2".to_string()), foo_dependency);
        let newer_foo_toml: toml::Value = toml! {
            [module]
            name = "foo"
            version = "1.0.2"
            module = "foo.wasm"
            description = ""
            [[command]]
            name = "do_more_foo_stuff" // COMMAND REMOVED AND ADDED
        };
        let newer_foo_manifest: Manifest = newer_foo_toml.try_into().unwrap();
        let newer_foo_dependency = Dependency {
            name: "foo".to_string(),
            manifest: newer_foo_manifest,
            download_url: "".to_string(),
        };
        map.insert(
            ("foo".to_string(), "1.0.2".to_string()),
            newer_foo_dependency,
        );
        // BAR package v 2.0.1
        let bar_toml: toml::Value = toml! {
            [module]
            name = "bar"
            version = "2.0.1"
            module = "bar.wasm"
            description = ""
        };
        let bar_manifest: Manifest = bar_toml.try_into().unwrap();
        let bar_dependency = Dependency {
            name: "foo".to_string(),
            manifest: bar_manifest,
            download_url: "".to_string(),
        };
        map.insert(("bar".to_string(), "2.0.1".to_string()), bar_dependency);
        // BAR package v 3.0.0
        let bar_newer_toml: toml::Value = toml! {
            [module]
            name = "bar"
            version = "3.0.0"
            module = "bar.wasm"
            description = ""
            [[command]]
            name = "do_bar_stuff" // ADDED COMMAND
        };
        let bar_newer_manifest: Manifest = bar_newer_toml.try_into().unwrap();
        let bar_newer_dependency = Dependency {
            name: "foo".to_string(),
            manifest: bar_newer_manifest,
            download_url: "".to_string(),
        };
        map.insert(
            ("bar".to_string(), "3.0.0".to_string()),
            bar_newer_dependency,
        );
        let test_resolver = TestResolver(map);

        // existing lockfile
        let wapm_lock_toml = toml! {
            [modules."foo 1.0.0"]
            name = "foo"
            version = "1.0.0"
            source = "registry+foo"
            resolved = ""
            integrity = ""
            hash = ""
            abi = "None"
            entry = "foo.wasm"
            [modules."bar 2.0.1"]
            name = "bar"
            version = "2.0.1"
            source = "registry+bar"
            resolved = ""
            integrity = ""
            hash = ""
            abi = "None"
            entry = "bar.wasm"
            [commands.do_foo_stuff]
            module = "foo 1.0.0"
        };

        let existing_lockfile: Lockfile = wapm_lock_toml.try_into().unwrap();

        let lockfile =
            Lockfile::new_from_manifest_and_lockfile(&manifest, existing_lockfile, &test_resolver)
                .unwrap();

        // existing lockfile
        let expected_lock_toml = toml! {
            [modules."foo 1.0.2"]
            name = "foo"
            version = "1.0.2"
            source = "registry+foo"
            resolved = ""
            integrity = ""
            hash = ""
            abi = "None"
            entry = "foo.wasm"
            [modules."bar 3.0.0"]
            name = "bar"
            version = "3.0.0"
            source = "registry+bar"
            resolved = ""
            integrity = ""
            hash = ""
            abi = "None"
            entry = "bar.wasm"
            [commands.do_more_foo_stuff]
            module = "foo 1.0.2"
            [commands.do_bar_stuff]
            module = "bar 3.0.0"
        };

        let expected_lockfile: Lockfile = expected_lock_toml.try_into().unwrap();

        assert_eq!(expected_lockfile, lockfile);
    }
}
