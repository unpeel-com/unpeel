mod install;
mod ui;
mod unpeel;

use std::path::PathBuf;

use unpeel_app_kit::Explorer;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    install::ensure_installed();
    let root = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or(std::env::current_dir()?);
    let explorer = Explorer::new(root)?;
    ui::run(explorer)?;
    Ok(())
}
