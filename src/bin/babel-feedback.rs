#![forbid(unsafe_code)]
#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

//! Private native command-feedback helper. Receives sanitized JSON on stdin.
use clap::Parser;

#[derive(Parser)]
struct Args {
    /// Render synthetic phase snapshots without opening a window or microphone.
    #[arg(long, hide = true)]
    preview_dir: Option<std::path::PathBuf>,
}

fn main() {
    let args = Args::parse();
    let result = match args.preview_dir {
        Some(directory) => babel_audio::feedback::renderer::render_previews(&directory),
        None => babel_audio::feedback::renderer::run(),
    };
    if result.is_err() {
        // The parent detects exit/failed readiness and uses system notifications.
        // Never print protocol contents, recognized speech, or private diagnostics.
        std::process::exit(1);
    }
}
