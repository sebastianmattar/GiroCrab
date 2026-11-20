//! `girocrab` – launcher for the native SwiftUI app.
//!
//! `cargo build` compiles the Rust core, the UniFFI Swift bindings and the
//! SwiftUI app (see build.rs). This binary just starts that app and forwards
//! all arguments, e.g.
//!
//!   cargo run -- --demo-seed
//!   cargo run -- --screenshot shots/now.png      (implies --demo-seed)
//!   GIROCRAB_DEMO_SEED=1 cargo run

use std::path::PathBuf;
use std::process::{Command, exit};

fn main() {
    let app = std::env::var("GIROCRAB_APP_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(env!("GIROCRAB_APP_BIN")));
    if !app.exists() {
        eprintln!(
            "GiroCrab app not found at {}.\nBuild on macOS with Xcode installed (`cargo build`), or open GiroCrabApp/Xcode/GiroCrab.xcodeproj.",
            app.display()
        );
        exit(1);
    }
    // Make relative screenshot paths relative to the caller's directory.
    let cwd = std::env::current_dir().unwrap_or_default();
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(i) = args.iter().position(|a| a == "--screenshot")
        && let Some(p) = args.get_mut(i + 1)
    {
        let pb = PathBuf::from(&*p);
        if pb.is_relative() {
            *p = cwd.join(pb).to_string_lossy().to_string();
        }
    }
    match Command::new(&app).args(&args).status() {
        Ok(s) => exit(s.code().unwrap_or(1)),
        Err(e) => {
            eprintln!("could not start {}: {e}", app.display());
            exit(1);
        }
    }
}
