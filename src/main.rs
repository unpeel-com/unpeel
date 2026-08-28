mod app;
mod git;
mod install;
mod ui;
mod unpeel;

use std::path::PathBuf;

use app::App;
use git::Repository;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    match std::env::args_os().nth(1).as_deref() {
        Some(argument) if argument == "--help" || argument == "-h" => {
            println!("Usage: unpeel-diffs [PATH]\n\nOpen the Git working tree containing PATH.");
            return Ok(());
        }
        Some(argument) if argument == "--version" => {
            println!("unpeel-diffs {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        Some(argument) if argument == "--register" => {
            install::ensure_installed();
            return Ok(());
        }
        _ => {}
    }

    install::ensure_installed();
    let start = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or(std::env::current_dir()?);
    let repository = Repository::discover(start)?;
    ui::run(App::new(repository)?)?;
    Ok(())
}
