//! One audio/controller process per configuration, independent of its HTTP port.
use std::{
    fs::File,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, ensure};

/// Hold until controller shutdown, not merely until the HTTP listener closes.
/// The sidecar is intentionally never deleted: unlinking a locked file would
/// allow a second process to lock a different inode at the same path.
pub struct InstanceGuard {
    _lock: File,
    config_path: PathBuf,
}

impl InstanceGuard {
    pub fn acquire(config: &Path) -> Result<Self> {
        let absolute = std::path::absolute(config).context("Invalid configuration path")?;
        let config = if absolute.exists() {
            absolute
                .canonicalize()
                .context("Could not locate the configuration")?
        } else {
            let parent = absolute
                .parent()
                .context("Configuration directory unavailable")?
                .canonicalize()
                .context("Configuration directory unavailable")?;
            parent.join(absolute.file_name().context("Invalid configuration name")?)
        };
        let mut name = config
            .file_name()
            .context("Invalid configuration name")?
            .to_os_string();
        name.push(".babel-instance.lock");
        let path = config.with_file_name(name);
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) => ensure!(
                metadata.is_file() && !metadata.is_symlink(),
                "The instance lock is not a regular file"
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).context("Could not inspect the instance lock");
            }
        }
        let mut options = std::fs::OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let lock = options
            .open(&path)
            .context("Could not create the instance lock alongside the configuration")?;
        ensure!(
            lock.metadata()?.is_file(),
            "The instance lock is not a regular file"
        );
        match lock.try_lock() {
            Ok(()) => Ok(Self {
                _lock: lock,
                config_path: config,
            }),
            Err(std::fs::TryLockError::WouldBlock) => anyhow::bail!(
                "Babel is already running with this configuration. Use the tray icon to open Settings or close the current instance before starting another."
            ),
            Err(std::fs::TryLockError::Error(error)) => {
                Err(error).context("Could not lock the Babel instance")
            }
        }
    }

    /// Loading, migration and atomic saves must all use the locked identity.
    /// Saving through the original symlink would replace the link itself and
    /// give the next launcher a different lock path for the same running app.
    pub fn config_path(&self) -> &Path {
        &self.config_path
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicate_configuration_is_blocked_until_its_owner_exits() {
        let directory = tempfile::tempdir().unwrap();
        let config = directory.path().join("Babel sessão.toml");
        let first = InstanceGuard::acquire(&config).unwrap();
        assert!(InstanceGuard::acquire(&config).is_err());
        // Atomic saves replace the config inode, never the separate lock.
        let replacement = directory.path().join("replacement.toml");
        std::fs::write(&replacement, "fixture").unwrap();
        std::fs::rename(&replacement, &config).unwrap();
        assert!(InstanceGuard::acquire(&config).is_err());
        let other = InstanceGuard::acquire(&directory.path().join("other.toml")).unwrap();
        drop(other);
        drop(first);
        let restarted = InstanceGuard::acquire(&config).unwrap();
        assert!(
            directory
                .path()
                .join("Babel sessão.toml.babel-instance.lock")
                .exists()
        );
        drop(restarted);
    }

    #[cfg(unix)]
    #[test]
    fn configuration_symlinks_share_the_same_lock() {
        let directory = tempfile::tempdir().unwrap();
        let config = directory.path().join("config.toml");
        std::fs::write(&config, "fixture").unwrap();
        let alias = directory.path().join("alias.toml");
        std::os::unix::fs::symlink(&config, &alias).unwrap();
        let _guard = InstanceGuard::acquire(&config).unwrap();
        assert!(InstanceGuard::acquire(&alias).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn loading_and_saving_an_alias_preserve_its_locked_identity() {
        let directory = tempfile::tempdir().unwrap();
        let target_directory = directory.path().join("settings");
        std::fs::create_dir(&target_directory).unwrap();
        let config = target_directory.join("config.toml");
        // Loading this old config triggers an atomic migration save.
        std::fs::write(&config, "version = 1\n[files]\nbase_path = '.'\n").unwrap();
        let alias = directory.path().join("alias.toml");
        std::os::unix::fs::symlink(&config, &alias).unwrap();
        let guard = InstanceGuard::acquire(&alias).unwrap();
        assert_eq!(guard.config_path(), config.canonicalize().unwrap());
        let mut loaded = crate::config::AppConfig::load(guard.config_path()).unwrap();
        assert_eq!(
            Path::new(&loaded.files.base_path),
            target_directory.canonicalize().unwrap()
        );
        loaded.interface.language = "en".into();
        loaded.save(guard.config_path()).unwrap();
        assert!(std::fs::symlink_metadata(&alias).unwrap().is_symlink());
        assert_eq!(
            crate::config::AppConfig::load(guard.config_path())
                .unwrap()
                .interface
                .language,
            "en"
        );
        assert!(InstanceGuard::acquire(&alias).is_err());
        assert!(InstanceGuard::acquire(&config).is_err());
        drop(guard);
        let restarted = InstanceGuard::acquire(&alias).unwrap();
        assert_eq!(restarted.config_path(), config.canonicalize().unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn lock_symlinks_are_rejected_without_touching_their_target() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("target");
        std::fs::write(&target, "unchanged").unwrap();
        std::os::unix::fs::symlink(
            &target,
            directory.path().join("config.toml.babel-instance.lock"),
        )
        .unwrap();
        assert!(InstanceGuard::acquire(&directory.path().join("config.toml")).is_err());
        assert_eq!(std::fs::read_to_string(target).unwrap(), "unchanged");
    }
}
