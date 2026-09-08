mod app;
mod git;
mod highlight;
mod install;
mod ui;

use std::path::PathBuf;

use app::App;
use git::Repository;
use unpeel_app_kit::AppContext;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    match std::env::args_os().nth(1).as_deref() {
        Some(argument) if argument == "--help" || argument == "-h" => {
            println!(
                "Usage: unpeel-diffs [PATH]\n\nBrowse Git changes and commit history for PATH."
            );
            return Ok(());
        }
        Some(argument) if argument == "--version" => {
            println!("unpeel-diffs {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        Some(argument) if argument == "--syntax-licenses" => {
            println!("{}", include_str!("../SYNTAX_NOTICES.txt"));
            for license in two_face::acknowledgement::listing().for_syntaxes() {
                println!("\n{}\n{}", license.rel_path.display(), license.text);
            }
            return Ok(());
        }
        _ => {}
    }

    let explicit_path = std::env::args_os().nth(1).map(PathBuf::from);
    let follow_agent_context = explicit_path.is_none();
    let app_context = AppContext::detect();
    let start = match explicit_path.or_else(|| app_context.current_root().map(PathBuf::from)) {
        Some(root) => root,
        None => std::env::current_dir()?,
    };
    let repository = Repository::discover(start)?;
    ui::run(App::new(repository)?, follow_agent_context, app_context)?;
    Ok(())
}
