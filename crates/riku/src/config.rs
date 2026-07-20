//! Filesystem shell for riku's user-level Config. The data model and TOML
//! interpretation stay pure in `cli`; this module only reads and writes bytes.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::cli::Config;

pub fn path() -> Result<PathBuf, String> {
    path_from(
        std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from),
        dirs::home_dir(),
    )
}

fn path_from(config_home: Option<PathBuf>, home: Option<PathBuf>) -> Result<PathBuf, String> {
    config_home
        .or_else(|| home.map(|home| home.join(".config")))
        .map(|dir| dir.join("riku/config.toml"))
        .ok_or_else(|| "could not determine the user config directory".to_string())
}

pub fn read(path: &Path) -> Result<Option<String>, String> {
    match fs::read_to_string(path) {
        Ok(contents) => Ok(Some(contents)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("could not read {}: {error}", path.display())),
    }
}

pub fn write(path: &Path, config: &Config) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("config path has no parent: {}", path.display()))?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("could not create {}: {error}", parent.display()))?;
    let contents = config.serialize()?;
    let mut options = OpenOptions::new();
    options.create(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .map_err(|error| format!("could not write {}: {error}", path.display()))?;
    set_private_permissions(path)?;
    file.set_len(0)
        .map_err(|error| format!("could not clear {}: {error}", path.display()))?;
    file.write_all(contents.as_bytes())
        .map_err(|error| format!("could not write {}: {error}", path.display()))?;
    file.sync_all()
        .map_err(|error| format!("could not sync {}: {error}", path.display()))?;
    Ok(())
}

#[cfg(unix)]
fn set_private_permissions(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .map_err(|error| format!("could not secure {}: {error}", path.display()))
}

#[cfg(not(unix))]
fn set_private_permissions(_: &Path) -> Result<(), String> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use crate::cli::Config;

    use super::{path_from, write};

    #[test]
    fn uses_the_specified_dot_config_location_by_default() {
        assert_eq!(
            path_from(None, Some("/Users/ada".into())).unwrap(),
            std::path::PathBuf::from("/Users/ada/.config/riku/config.toml")
        );
    }

    #[cfg(unix)]
    #[test]
    fn writes_a_private_config_file() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("riku/config.toml");
        write(&path, &Config::default()).unwrap();
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}
