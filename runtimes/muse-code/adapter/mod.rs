use super::{shared, Integration};
use crate::session_host::SessionHostLaunch;
use portable_pty::CommandBuilder;

mod resume {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../runtimes/muse-code/adapter/resume.rs"
    ));
}

pub(crate) mod setup {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../runtimes/muse-code/adapter/setup.rs"
    ));
}

fn configure_host_command(
    _launch: &SessionHostLaunch,
    cmd: &mut CommandBuilder,
    shell_prelude: &mut Vec<String>,
) -> Result<(), String> {
    // Muse Code loads native plugins — the vehicle for Unpeel's lifecycle
    // hooks (install_muse_hooks) — only when the experimental plugins gate is
    // set in the environment. Without it the installed unpeel plugin sits
    // inert and the session never reports busy/idle/attention.
    cmd.env("MUSE_EXPERIMENTAL_PLUGINS", "1");
    shell_prelude.push(format!(
        "export MUSE_EXPERIMENTAL_PLUGINS={}",
        shared::shell_quote("1")
    ));
    Ok(())
}

pub(crate) const INTEGRATION: Integration = Integration::new(
    Some(setup::install_muse_hooks),
    Some(configure_host_command),
)
.with_resume_adapter(resume::ADAPTER);

#[cfg(test)]
mod tests {
    use super::configure_host_command;
    use portable_pty::CommandBuilder;

    #[test]
    fn exports_the_plugin_gate_for_hook_delivery() {
        let launch: super::SessionHostLaunch = serde_json::from_value(serde_json::json!({
            "session": {
                "id": "muse-session",
                "project_id": "test-project",
                "label": "Muse",
                "command": "muse --yolo"
            },
            "cwd": "/tmp",
            "dark_mode": null,
            "hook_port": 4321
        }))
        .expect("launch fixture");
        let mut command = CommandBuilder::new("true");
        let mut prelude = Vec::new();
        configure_host_command(&launch, &mut command, &mut prelude).expect("configure");
        assert!(prelude
            .join("\n")
            .contains("export MUSE_EXPERIMENTAL_PLUGINS='1'"));
    }
}
