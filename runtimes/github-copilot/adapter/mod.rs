use super::Integration;
use crate::session_host::SessionHostLaunch;
use portable_pty::CommandBuilder;

mod resume {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../runtimes/github-copilot/adapter/resume.rs"
    ));
}

pub(crate) mod setup {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../runtimes/github-copilot/adapter/setup.rs"
    ));
}

// The shared integration callback contract uses Vec because most providers
// append shell prelude entries; Copilot happens not to need it.
#[allow(clippy::ptr_arg)]
fn configure_host_command(
    launch: &SessionHostLaunch,
    _cmd: &mut CommandBuilder,
    _shell_prelude: &mut Vec<String>,
) -> Result<(), String> {
    setup::prepare_copilot_project_hooks(&launch.cwd)
}

pub(crate) const INTEGRATION: Integration = Integration::new(
    Some(setup::install_copilot_hook),
    Some(configure_host_command),
)
.with_resume_adapter(resume::ADAPTER);
