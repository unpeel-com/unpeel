//! Opt-in real-server probe for the native Controller's system-SSH modes.
//! No target or credential is compiled into the test or printed on failure.

use std::sync::Arc;

use unpeel_core::remote_session_backend::RemoteSessionBackend;
use unpeel_core::ssh_connection::{
    SshAskpass, SshConnectionOptions, SshHostConnection, SshLaunchMode, SshTarget,
};

#[test]
#[ignore = "requires an explicitly configured real SSH Host"]
fn native_automatic_modes_bootstrap_a_real_host() {
    let target = std::env::var("UNPEEL_LIVE_SSH_TARGET")
        .expect("set UNPEEL_LIVE_SSH_TARGET=ssh://user@host");
    let secret = std::env::var("UNPEEL_LIVE_SSH_SECRET").ok();
    let backend = |mode| {
        let askpass = secret
            .clone()
            .map(|secret| SshAskpass::new(env!("CARGO_BIN_EXE_unpeel-host"), secret).unwrap());
        let connection = SshHostConnection::with_options(
            SshTarget::parse(&target).unwrap(),
            SshConnectionOptions {
                launch_mode: mode,
                askpass,
            },
        )
        .unwrap();
        RemoteSessionBackend::new(Arc::new(connection))
    };
    let standard = backend(SshLaunchMode::Command);
    let (bootstrap, accepted) = match standard.bootstrap() {
        Ok(bootstrap) => (bootstrap, standard),
        Err(_) => {
            standard.disconnect();
            let interactive = backend(SshLaunchMode::InteractiveShell);
            let bootstrap = interactive
                .bootstrap()
                .expect("real interactive SSH Host bootstrap");
            (bootstrap, interactive)
        }
    };
    assert!(bootstrap.snapshot.host_id.is_some());
    assert!(bootstrap.snapshot.host_protocol.is_some());
    accepted.disconnect();
}
