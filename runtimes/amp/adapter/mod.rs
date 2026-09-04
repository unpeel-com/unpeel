use super::Integration;
use crate::session_host::SessionHostLaunch;
use portable_pty::CommandBuilder;

mod resume {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../runtimes/amp/adapter/resume.rs"
    ));
}

pub(crate) mod setup {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../runtimes/amp/adapter/setup.rs"
    ));
}

fn configure_host_command(
    launch: &SessionHostLaunch,
    cmd: &mut CommandBuilder,
    shell_prelude: &mut Vec<String>,
) -> Result<(), String> {
    setup::prepare_amp_project_plugin(&launch.cwd)?;
    cmd.env("PLUGINS", "all");
    shell_prelude.push("export PLUGINS=all".to_string());
    Ok(())
}

pub(crate) const INTEGRATION: Integration = Integration::new(
    Some(setup::install_amp_plugin),
    Some(configure_host_command),
)
.with_resume_adapter(resume::ADAPTER);
