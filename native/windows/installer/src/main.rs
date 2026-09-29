//! Own-device installation policy in safe Rust; Windows FFI is isolated in win32.
#![deny(unsafe_code)]

use anyhow::{Result, ensure};
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[cfg(windows)]
#[allow(unsafe_code)]
mod win32;

const HARDWARE_ID: &str = "ROOT\\BabelAudio";

#[derive(Parser)]
#[command(
    version,
    about = "Install/remove only Babel Audio. Install/remove require an already elevated terminal; this tool never changes boot security or installs certificates."
)]
struct Args {
    #[command(subcommand)]
    command: Action,
}

#[derive(Subcommand)]
enum Action {
    /// Validate the signed INF, create its root device and install/update it.
    Install {
        #[arg(long)]
        inf: PathBuf,
    },
    /// Remove only the exact Babel root device and its unused OEM driver package.
    Remove {
        /// Signed source INF used to finish package cleanup after device removal/reboot.
        #[arg(long)]
        inf: Option<PathBuf>,
    },
    /// Read-only enumeration of Babel root devices, including disconnected ones.
    List,
    /// Succeed only when no Babel root device exists; safe for app-uninstall checks.
    CheckAbsent,
}

fn is_own_hardware(ids: &[String]) -> bool {
    ids.len() == 1 && ids[0].eq_ignore_ascii_case(HARDWARE_ID)
}

fn oem_inf(name: &str) -> Result<&str> {
    let name_lower = name.to_ascii_lowercase();
    let digits = name_lower
        .strip_prefix("oem")
        .and_then(|rest| rest.strip_suffix(".inf"));
    ensure!(
        digits.is_some_and(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit())),
        "Refusing to remove a driver package without an exact OEM INF identity"
    );
    Ok(name)
}

fn main() -> Result<()> {
    let args = Args::parse();
    #[cfg(windows)]
    {
        let report = win32::execute(args.command)?;
        println!("{}", serde_json::to_string_pretty(&report)?);
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = args;
        let _ = (is_own_hardware(&[]), oem_inf(""));
        anyhow::bail!("The installer runs only on Windows; portable policy tests can run here")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn check_absent_is_a_read_only_command_without_an_inf_argument() {
        assert!(matches!(
            Args::try_parse_from(["installer", "check-absent"])
                .unwrap()
                .command,
            Action::CheckAbsent
        ));
        assert!(
            Args::try_parse_from(["installer", "check-absent", "--inf", "BabelAudio.inf"]).is_err()
        );
    }
    #[test]
    fn hardware_ownership_is_exact_and_never_a_prefix_or_friendly_name() {
        assert!(is_own_hardware(&["root\\babelaudio".into()]));
        for ids in [
            vec![],
            vec!["ROOT\\BabelAudioOther".into()],
            vec!["Babel Audio v1".into()],
            vec![HARDWARE_ID.into(), "other".into()],
        ] {
            assert!(!is_own_hardware(&ids));
        }
    }
    #[test]
    fn removing_oem_packages_cannot_escape_or_select_a_builtin_inf() {
        for valid in ["oem0.inf", "OEM123.INF"] {
            assert!(oem_inf(valid).is_ok());
        }
        for bad in [
            "oem.inf",
            "wdmaudio.inf",
            "BabelAudio.inf",
            "../oem4.inf",
            "C:\\oem4.inf",
            "oem1.inf\0",
            "oem-1.inf",
        ] {
            assert!(oem_inf(bad).is_err());
        }
    }
}
