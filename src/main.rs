mod install;
mod ui;

use std::ffi::{OsStr, OsString};
use std::io;
use std::path::PathBuf;

use unpeel_app_kit::Explorer;

const HELP: &str = "\
unpeel-filetree — borderless project file explorer

Usage:
  unpeel-filetree [OPTIONS] [PATH]

Options:
  -e, --ext EXT       Show EXT files and folders containing them; repeatable
                      and comma-separated values are accepted
      --version       Print the App version
  -h, --help          Print this help

Examples:
  unpeel-filetree --ext md .
  unpeel-filetree ~/Notes --ext md --ext mdx
";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Options {
    root: Option<PathBuf>,
    extensions: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ParsedArgs {
    Run(Options),
    Help,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    match std::env::args_os().nth(1).as_deref() {
        Some(argument) if argument == OsStr::new("--version") => {
            println!("unpeel-filetree {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        _ => {}
    }
    let options = match parse_args(std::env::args_os().skip(1))
        .map_err(|message| io::Error::new(io::ErrorKind::InvalidInput, message))?
    {
        ParsedArgs::Help => {
            print!("{HELP}");
            return Ok(());
        }
        ParsedArgs::Run(options) => options,
    };
    let follow_agent_context = options.root.is_none();
    let root = options.root.unwrap_or(std::env::current_dir()?);
    let mut explorer = Explorer::scoped(root)?;
    if !options.extensions.is_empty() {
        explorer.set_prune_unmatched_directories(true)?;
        explorer.set_file_extensions(options.extensions)?;
    }
    ui::run(explorer, follow_agent_context)?;
    Ok(())
}

fn parse_args<I, T>(args: I) -> Result<ParsedArgs, String>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString>,
{
    let mut args = args.into_iter().map(Into::into).peekable();
    let mut options = Options::default();
    let mut positional_only = false;
    while let Some(argument) = args.next() {
        if !positional_only && argument == OsStr::new("--") {
            positional_only = true;
            continue;
        }
        if !positional_only {
            let text = argument.to_str();
            match text {
                Some("-h" | "--help") => return Ok(ParsedArgs::Help),
                Some("-e" | "--ext" | "--extension") => {
                    let value = args
                        .next()
                        .ok_or_else(|| format!("{argument:?} requires an extension"))?;
                    push_extensions(&value, &mut options.extensions)?;
                    continue;
                }
                Some(value) if value.starts_with("--ext=") => {
                    push_extensions(
                        OsStr::new(value.trim_start_matches("--ext=")),
                        &mut options.extensions,
                    )?;
                    continue;
                }
                Some(value) if value.starts_with("--extension=") => {
                    push_extensions(
                        OsStr::new(value.trim_start_matches("--extension=")),
                        &mut options.extensions,
                    )?;
                    continue;
                }
                Some(value) if value.starts_with('-') => {
                    return Err(format!("unknown option: {value}\n\n{HELP}"));
                }
                _ => {}
            }
        }
        if options.root.replace(PathBuf::from(&argument)).is_some() {
            return Err(format!("only one PATH may be supplied\n\n{HELP}"));
        }
    }
    Ok(ParsedArgs::Run(options))
}

fn push_extensions(value: &OsStr, extensions: &mut Vec<String>) -> Result<(), String> {
    let value = value
        .to_str()
        .ok_or_else(|| "extensions must be valid UTF-8".to_string())?;
    let mut added = false;
    for part in value.split(',') {
        let extension = part.trim().trim_start_matches('.').to_ascii_lowercase();
        if extension.is_empty() || extension.contains(['/', '\\']) {
            return Err(format!("invalid extension: {part:?}"));
        }
        added = true;
        if !extensions.contains(&extension) {
            extensions.push(extension);
        }
    }
    if !added {
        return Err("an extension cannot be empty".to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extension_flags_are_repeatable_comma_separated_and_normalized() {
        assert_eq!(
            parse_args(["--ext", ".MD,mdx", "project", "-e", "markdown"]).unwrap(),
            ParsedArgs::Run(Options {
                root: Some(PathBuf::from("project")),
                extensions: vec!["md".to_string(), "mdx".to_string(), "markdown".to_string()],
            })
        );
    }

    #[test]
    fn flags_can_follow_the_path_and_double_dash_allows_a_dashed_path() {
        assert_eq!(
            parse_args(["project", "--extension=.md"]).unwrap(),
            ParsedArgs::Run(Options {
                root: Some(PathBuf::from("project")),
                extensions: vec!["md".to_string()],
            })
        );
        assert_eq!(
            parse_args(["--", "-notes"]).unwrap(),
            ParsedArgs::Run(Options {
                root: Some(PathBuf::from("-notes")),
                extensions: Vec::new(),
            })
        );
    }

    #[test]
    fn malformed_arguments_fail_with_a_useful_message() {
        assert!(parse_args(["--ext"]).unwrap_err().contains("requires"));
        assert!(parse_args(["--ext", ""]).unwrap_err().contains("invalid"));
        assert!(
            parse_args(["--wat"])
                .unwrap_err()
                .contains("unknown option")
        );
        assert!(
            parse_args(["one", "two"])
                .unwrap_err()
                .contains("only one PATH")
        );
        assert_eq!(parse_args(["--help"]).unwrap(), ParsedArgs::Help);
    }
}
