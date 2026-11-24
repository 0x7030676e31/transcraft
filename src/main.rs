#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod bundle;
mod config;
mod launch;
mod ui;

use anyhow::Result;

fn main() {
    if let Err(error) = start() {
        eprintln!("{error:#}");
        // GUI builds on Windows have no console; retain startup failures on disk.
        if let Ok(root) = config::data_root() {
            let _ = std::fs::create_dir_all(&root);
            let _ = std::fs::write(root.join("launcher-error.log"), format!("{error:#}"));
        }
        std::process::exit(1);
    }
}

fn start() -> Result<()> {
    let args: Vec<_> = std::env::args_os().collect();
    if args.iter().any(|s| s == "--version") {
        println!("Transcraft {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if args.iter().any(|s| s == "--help") {
        println!(
            "Transcraft {}\n\nDefault: graphical launcher\n--data-dir PATH   Override the user data folder\n--install         Extract and verify the offline bundle\n--verify          Install if needed, then verify all bundled files\n--launch          Launch using saved settings without opening the GUI\n--version         Print launcher version",
            env!("CARGO_PKG_VERSION")
        );
        return Ok(());
    }
    let root = config::data_root()?;
    if args.iter().any(|s| s == "--install" || s == "--verify") {
        let _guard = bundle::lock(&root, "game.lock")?;
        let (mut file, payload) = bundle::embedded()?;
        let dest = payload.install(&mut file, &root, false, &|s| println!("{s}"))?;
        if args.iter().any(|s| s == "--verify") {
            bundle::verify(&dest)?;
        }
        println!("Bundle ready: {}", dest.display());
        return Ok(());
    }
    if args.iter().any(|s| s == "--launch") {
        return launch::run(&root, &config::Settings::load(&root)?, &|s| println!("{s}"));
    }
    ui::run(root)
}
