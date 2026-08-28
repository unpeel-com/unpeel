mod app;
mod git;
mod highlight;
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
    let explicit_path = std::env::args_os().nth(1).map(PathBuf::from);
    let follow_agent_context = explicit_path.is_none();
    let start = explicit_path.unwrap_or(std::env::current_dir()?);
    let repository = Repository::discover(start)?;
    ui::run(App::new(repository)?, follow_agent_context)?;
    Ok(())
}
