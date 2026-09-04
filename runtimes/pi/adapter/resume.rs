use crate::resume::{
    has_any_flag, has_resume_flag, id_in_command, join, quoted, strip_resume_flags, tokenize,
    unquote, with_flag, NewLaunchContext, PreparedNewLaunch, ResumeAdapter,
};
use std::path::{Component, Path};

const RESUME_FLAGS: &[(&str, bool)] = &[
    ("-c", false),
    ("--continue", false),
    ("-r", true),
    ("--resume", true),
    ("--session", true),
];
const ID_FLAGS: &[&str] = &["--session", "--resume", "-r"];
const PIN_FLAGS: &[&str] = &[
    "-c",
    "--continue",
    "-r",
    "--resume",
    "--session",
    "--session-dir",
    "--no-session",
    "--fork",
];

fn resumed(command: &str, provider_session_id: Option<&str>) -> String {
    let tokens = tokenize(command);
    let has_resume_marker = has_resume_flag(&tokens, RESUME_FLAGS);
    let id = provider_session_id
        .map(str::to_string)
        .filter(|id| !id.is_empty())
        .or_else(|| id_in_command(&tokens, ID_FLAGS));
    let stripped = strip_resume_flags(tokens, RESUME_FLAGS);
    match id {
        Some(id) => join(with_flag(stripped, &["--session", &quoted(&id)])),
        None if has_resume_marker => command.trim().to_string(),
        // A managed `--session-dir`, when present, stays in `stripped` and
        // makes continue exact by construction.
        None => join(with_flag(stripped, &["--continue"])),
    }
}

fn fresh(command: &str) -> String {
    join(strip_resume_flags(tokenize(command), RESUME_FLAGS))
}

fn prepare_new_launch(command: &str, context: NewLaunchContext<'_>) -> PreparedNewLaunch {
    let managed_path = context
        .managed_storage_path_override
        .map(str::to_string)
        .or_else(|| {
            let session_id = context.session_id?;
            let mut components = Path::new(session_id).components();
            if !matches!(components.next(), Some(Component::Normal(_)))
                || components.next().is_some()
            {
                return None;
            }
            Some(
                context
                    .unpeel_home?
                    .join("pi-sessions")
                    .join(session_id)
                    .to_string_lossy()
                    .to_string(),
            )
        });
    let Some(managed_path) = managed_path else {
        return PreparedNewLaunch::unchanged(command);
    };
    let trimmed = command.trim();
    let tokens = tokenize(trimmed);
    if has_any_flag(&tokens, PIN_FLAGS) {
        return PreparedNewLaunch::unchanged(trimmed);
    }
    PreparedNewLaunch {
        command: join(with_flag(
            tokens,
            &["--session-dir", &quoted(&managed_path)],
        )),
        provider_session_id: None,
        managed_storage_path: Some(managed_path),
    }
}

fn managed_session_dir(command: &str, root: &str) -> Option<String> {
    let tokens = tokenize(command);
    let directory = tokens
        .windows(2)
        .find(|pair| pair[0] == "--session-dir")
        .map(|pair| unquote(&pair[1]))?;
    let relative = Path::new(&directory).strip_prefix(Path::new(root)).ok()?;
    let mut components = relative.components();
    let first = components.next()?;
    if !matches!(first, Component::Normal(_))
        || components.any(|component| !matches!(component, Component::Normal(_)))
    {
        return None;
    }
    Some(directory)
}

pub(super) const ADAPTER: ResumeAdapter = ResumeAdapter::new(resumed, fresh)
    .with_new_launch_preparation(prepare_new_launch)
    .with_managed_session_dir(managed_session_dir);

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn new_session_storage_is_pinned_and_survives_resume() {
        let prepared = prepare_new_launch(
            "pi --yolo",
            NewLaunchContext {
                session_id: Some("s1"),
                unpeel_home: Some(Path::new("/root/.unpeel")),
                managed_storage_path_override: None,
            },
        );
        assert_eq!(
            prepared.command,
            "pi --yolo --session-dir '/root/.unpeel/pi-sessions/s1'"
        );
        assert_eq!(
            prepared.managed_storage_path.as_deref(),
            Some("/root/.unpeel/pi-sessions/s1")
        );
        assert_eq!(
            resumed(&prepared.command, None),
            "pi --yolo --session-dir '/root/.unpeel/pi-sessions/s1' --continue"
        );
    }

    #[test]
    fn managed_storage_rejects_path_traversal() {
        let prepared = prepare_new_launch(
            "pi",
            NewLaunchContext {
                session_id: Some("../escape"),
                unpeel_home: Some(Path::new("/root/.unpeel")),
                managed_storage_path_override: None,
            },
        );
        assert_eq!(prepared, PreparedNewLaunch::unchanged("pi"));
        assert_eq!(
            managed_session_dir(
                "pi --session-dir '/root/.unpeel/pi-sessions/../escape'",
                "/root/.unpeel/pi-sessions"
            ),
            None
        );
    }

    #[test]
    fn legacy_resume_markers_are_removed_and_never_re_pinned() {
        assert_eq!(
            resumed("pi -r old -c --yolo", Some("new")),
            "pi --yolo --session 'new'"
        );
        assert_eq!(
            fresh("pi -c --resume=old --session stale --yolo"),
            "pi --yolo"
        );
        assert_eq!(
            resumed("pi --resume latest --yolo", None),
            "pi --resume latest --yolo"
        );
        for command in ["pi -r old", "pi --session-dir=/tmp/custom"] {
            assert_eq!(
                prepare_new_launch(
                    command,
                    NewLaunchContext {
                        session_id: Some("s1"),
                        unpeel_home: Some(Path::new("/root/.unpeel")),
                        managed_storage_path_override: None,
                    }
                ),
                PreparedNewLaunch::unchanged(command),
                "must not pin over {command}"
            );
        }
    }
}
