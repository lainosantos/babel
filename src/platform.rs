//! Capabilities of the Babel process, independent of the browser's user agent.

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct PlatformInfo {
    pub os: &'static str,
    pub name: &'static str,
    pub audio_backend: &'static str,
    pub manages_virtual_devices: bool,
    pub autostart_method: &'static str,
}

impl PlatformInfo {
    pub fn current() -> Self {
        Self::for_os(std::env::consts::OS)
    }

    fn for_os(os: &str) -> Self {
        match os {
            "linux" => Self {
                os: "linux",
                name: "Linux",
                audio_backend: "pulseaudio",
                manages_virtual_devices: true,
                autostart_method: "xdg",
            },
            "macos" => Self {
                os: "macos",
                name: "macOS",
                audio_backend: "coreaudio",
                manages_virtual_devices: false,
                autostart_method: "launch_agent",
            },
            "windows" => Self {
                os: "windows",
                name: "Windows",
                audio_backend: "wasapi",
                manages_virtual_devices: false,
                autostart_method: "registry_run",
            },
            _ => Self {
                os: "unknown",
                name: "Unknown",
                audio_backend: "unsupported",
                manages_virtual_devices: false,
                autostart_method: "unsupported",
            },
        }
    }

    /// Keep the complete guide as the source of truth while serving just the
    /// relevant setup section. Missing sections never fall back to another OS.
    pub fn device_guide(self) -> &'static str {
        let heading = match self.os {
            "linux" => "## Linux:",
            "macos" => "## macOS:",
            "windows" => "## Windows:",
            _ => return "Sistema não suportado. Consulte /help/platforms/all.\n",
        };
        let full_guide = include_str!("../docs/platforms.md");
        let Some(start) = full_guide.find(heading) else {
            return "Guia indisponível. Consulte /help/platforms/all.\n";
        };
        let section = &full_guide[start..];
        let end = section.find("\n## ").unwrap_or(section.len());
        &section[..end]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guides_only_show_setup_for_the_selected_host() {
        for os in ["linux", "macos", "windows"] {
            let platform = PlatformInfo::for_os(os);
            let guide = platform.device_guide();
            assert!(guide.starts_with(&format!("## {}:", platform.name)));
            for other in ["linux", "macos", "windows"] {
                if os != other {
                    let other = PlatformInfo::for_os(other);
                    assert!(!guide.contains(&format!("## {}:", other.name)));
                }
            }
        }
        assert!(!PlatformInfo::for_os("unrecognized").manages_virtual_devices);
        assert!(
            PlatformInfo::for_os("unrecognized")
                .device_guide()
                .contains("não suportado")
        );
    }
}
