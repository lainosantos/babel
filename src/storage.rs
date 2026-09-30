//! Session destinations resolved against an explicit, absolute base directory.
//! Resolution never expands shell variables or `~`, creates directories, or
//! canonicalizes symlinks; the preview also works before a directory exists.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use serde::Serialize;

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct FilePaths {
    pub base_path: PathBuf,
    pub transcription_directory: PathBuf,
    pub recording_directory: PathBuf,
}

pub fn resolve(
    base_path: &str,
    transcription_directory: &str,
    recording_directory: &str,
) -> Result<FilePaths> {
    let base_path = resolve_base(base_path)?;
    let paths = FilePaths {
        transcription_directory: resolve_directory(&base_path, transcription_directory)?,
        recording_directory: resolve_directory(&base_path, recording_directory)?,
        base_path,
    };
    // JSON represents paths as strings. Report an actionable preview error
    // instead of failing later during response serialization or losing bytes.
    for path in [
        &paths.base_path,
        &paths.transcription_directory,
        &paths.recording_directory,
    ] {
        ensure!(
            path.to_str().is_some(),
            "The folder preview requires paths representable in UTF-8"
        );
    }
    Ok(paths)
}

pub(crate) fn default_base_path() -> String {
    base_from_home(std::env::home_dir())
}

fn base_from_home(home: Option<PathBuf>) -> String {
    home.filter(|path| path.is_absolute())
        .and_then(|path| path.join("Babel").into_os_string().into_string().ok())
        // An unavailable or unrepresentable home requires an explicit choice;
        // validation rejects this value without falling back to any cwd.
        .unwrap_or_default()
}

fn validate_path_value(value: &str, label: &str) -> Result<()> {
    ensure!(
        !value.trim().is_empty() && value.len() <= 4096 && !value.contains('\0'),
        "Invalid {label}: enter a path of up to 4096 bytes"
    );
    Ok(())
}

pub(crate) fn validate_path(value: &str, label: &str) -> Result<()> {
    validate_path_value(value, label)?;
    #[cfg(windows)]
    {
        let path = Path::new(value);
        let prefix = matches!(
            path.components().next(),
            Some(std::path::Component::Prefix(_))
        );
        ensure!(
            path.is_absolute() || (!path.has_root() && !prefix),
            "Ambiguous {label}: use a relative path, a complete drive path (C:\\folder) or UNC"
        );
    }
    Ok(())
}

pub(crate) fn resolve_base(base_path: &str) -> Result<PathBuf> {
    validate_path_value(base_path, "base folder")?;
    let path = Path::new(base_path);
    ensure!(
        path.is_absolute(),
        "The base folder must be an absolute path"
    );
    Ok(path.to_path_buf())
}

pub(crate) fn resolve_directory(base_path: &Path, directory: &str) -> Result<PathBuf> {
    validate_path(directory, "file folder")?;
    ensure!(base_path.is_absolute(), "The base folder must be absolute");
    join_absolute(base_path, directory)
}

fn join_absolute(anchor: &Path, path: &str) -> Result<PathBuf> {
    let path = Path::new(path);
    if path.is_absolute() {
        // An explicit absolute folder remains independent of the base setting.
        return Ok(path.to_path_buf());
    }
    std::path::absolute(anchor.join(path)).context("Could not resolve the file folder")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_destinations_use_only_the_explicit_base_without_creating_files() {
        let directory = tempfile::tempdir().unwrap();
        let base = directory.path().join("sessões");
        let paths = resolve(base.to_str().unwrap(), "textos", "áudio").unwrap();
        assert_eq!(paths.base_path, base);
        assert_eq!(paths.transcription_directory, base.join("textos"));
        assert_eq!(paths.recording_directory, base.join("áudio"));
        assert!(!base.exists());
        let json = serde_json::to_value(paths).unwrap();
        assert!(json.get("working_directory").is_none());
        assert!(Path::new(json["recording_directory"].as_str().unwrap()).is_absolute());
    }

    #[test]
    fn absolute_destinations_remain_independent_of_the_base() {
        let directory = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let paths = resolve(
            directory.path().to_str().unwrap(),
            "transcripts",
            other.path().to_str().unwrap(),
        )
        .unwrap();
        assert_eq!(paths.base_path, directory.path());
        assert_eq!(
            paths.transcription_directory,
            directory.path().join("transcripts")
        );
        assert_eq!(paths.recording_directory, other.path());
        let paths = resolve(
            other.path().to_str().unwrap(),
            directory.path().to_str().unwrap(),
            "recordings",
        )
        .unwrap();
        assert_eq!(paths.transcription_directory, directory.path());
    }

    #[test]
    fn folder_names_do_not_expand_shell_syntax_or_trim_meaningful_spaces() {
        let directory = tempfile::tempdir().unwrap();
        let base = directory.path().join("~");
        let paths = resolve(base.to_str().unwrap(), "$HOME", " final ").unwrap();
        assert_eq!(paths.base_path, base);
        assert_eq!(paths.transcription_directory, base.join("$HOME"));
        // Normal Win32 paths remove a trailing space during absolute-path
        // resolution. The leading space is meaningful on every supported OS.
        #[cfg(windows)]
        assert_eq!(paths.recording_directory, base.join(" final"));
        #[cfg(not(windows))]
        assert_eq!(paths.recording_directory, base.join(" final "));
    }

    #[test]
    fn invalid_paths_and_all_relative_bases_are_rejected_before_io() {
        let directory = tempfile::tempdir().unwrap();
        let base = directory.path().to_str().unwrap();
        for invalid in [
            String::new(),
            " \t\n".into(),
            "nul\0byte".into(),
            "a".repeat(4097),
        ] {
            assert!(resolve(&invalid, "text", "audio").is_err());
            assert!(resolve(base, &invalid, "audio").is_err());
            assert!(resolve(base, "text", &invalid).is_err());
        }
        for relative in [".", "..", "sessions", "~/Babel", "$HOME/Babel"] {
            assert!(resolve(relative, "text", "audio").is_err());
        }
    }

    #[test]
    fn default_base_never_falls_back_to_a_working_directory() {
        assert!(resolve_base(&base_from_home(None)).is_err());
        assert!(resolve_base(&base_from_home(Some(PathBuf::from("relative-home")))).is_err());
        let home = tempfile::tempdir().unwrap();
        assert_eq!(
            Path::new(&base_from_home(Some(home.path().to_path_buf()))),
            home.path().join("Babel")
        );
    }

    #[cfg(unix)]
    #[test]
    fn parent_segments_after_symlinks_keep_filesystem_semantics() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(directory.path().join("real/nested")).unwrap();
        std::os::unix::fs::symlink("real/nested", directory.path().join("alias")).unwrap();
        let base = directory.path().join("alias/../archive");
        let paths = resolve(base.to_str().unwrap(), "text", "audio").unwrap();
        assert_eq!(paths.base_path, base);
        std::fs::create_dir_all(&paths.transcription_directory).unwrap();
        std::fs::write(paths.transcription_directory.join("proof.txt"), "original").unwrap();
        assert!(
            directory
                .path()
                .join("real/archive/text/proof.txt")
                .is_file()
        );
        assert!(!directory.path().join("archive").exists());
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_home_requires_an_explicit_valid_base_instead_of_losing_bytes() {
        use std::os::unix::ffi::OsStringExt;
        let directory = tempfile::tempdir().unwrap();
        let home = directory
            .path()
            .join(std::ffi::OsString::from_vec(vec![0xff]));
        assert!(resolve_base(&base_from_home(Some(home))).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn windows_rejects_drive_state_paths_and_accepts_complete_drives_and_unc() {
        for invalid in [r"C:relative", "C:", r"\relative", "/relative"] {
            assert!(resolve(invalid, "text", "audio").is_err());
            assert!(resolve(r"C:\Babel", invalid, "audio").is_err());
            assert!(resolve(r"C:\Babel", "text", invalid).is_err());
        }
        for absolute in [
            r"D:\Sessions",
            r"\\server\share\Sessions",
            r"\\?\C:\Sessions",
            r"\\?\UNC\server\share\Sessions",
        ] {
            let paths = resolve(absolute, "text", absolute).unwrap();
            assert_eq!(paths.base_path, Path::new(absolute));
            assert_eq!(paths.recording_directory, Path::new(absolute));
        }
    }
}
