use super::{shared, Integration};
use crate::session_host::SessionHostLaunch;
use portable_pty::CommandBuilder;

mod resume {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../runtimes/opencode/adapter/resume.rs"
    ));
}

pub(crate) mod setup {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../runtimes/opencode/adapter/setup.rs"
    ));
}

fn configure_host_command(
    _launch: &SessionHostLaunch,
    cmd: &mut CommandBuilder,
    shell_prelude: &mut Vec<String>,
) -> Result<(), String> {
    let config_dir = setup::opencode_config_dir();
    let config_dir_str = config_dir.to_string_lossy().to_string();
    cmd.env("OPENCODE_CONFIG_DIR", &config_dir_str);
    shell_prelude.push(format!(
        "export OPENCODE_CONFIG_DIR={}",
        shared::shell_quote(&config_dir_str)
    ));
    Ok(())
}

pub(crate) const INTEGRATION: Integration = Integration::new(
    Some(setup::install_opencode_plugin),
    Some(configure_host_command),
)
.with_resume_adapter(resume::ADAPTER);
