//! `unpeel serve install|uninstall|status` — wrap the checked-in service
//! unit templates (`packaging/service/`) around the resolved binary and the
//! platform's per-user service manager.
//!
//! Deliberate boundaries:
//! - per-user only (LaunchAgent / `systemctl --user`): the service owns
//!   `~/.unpeel`, the user Keychain, and the per-user machine lease;
//! - `uninstall` stops the service and removes the unit file, nothing else —
//!   workspace data and running Session PTYs are never touched (Sessions
//!   survive a service stop by design);
//! - the manager binaries are resolved through `PATH`, so tests drive these
//!   flows with fake `launchctl`/`systemctl` shims and never register a real
//!   system service.

use std::path::{Path, PathBuf};
use std::process::Command;

const LAUNCHD_TEMPLATE: &str = include_str!("../../../packaging/service/com.unpeel.serve.plist");
const SYSTEMD_TEMPLATE: &str = include_str!("../../../packaging/service/unpeel-serve.service");
/// `--graphical`: the same service bound to `graphical-session.target` so it
/// runs inside the desktop session (Computer Use needs the display and the
/// session bus). The template documents why the session, not the unit,
/// imports DISPLAY into the user manager.
const SYSTEMD_GRAPHICAL_TEMPLATE: &str =
    include_str!("../../../packaging/service/unpeel-serve-graphical.service");
/// The line that identifies an installed graphical unit on re-read.
const GRAPHICAL_MARKER: &str = "PartOf=graphical-session.target";
/// The path the verbatim templates ship with; rendering rewrites it.
const TEMPLATE_BINARY: &str = "/usr/local/bin/unpeel";
const LAUNCHD_LABEL: &str = "com.unpeel.serve";
const SYSTEMD_UNIT: &str = "unpeel-serve";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ServiceManager {
    Launchd,
    Systemd,
}

impl ServiceManager {
    /// Platform default, overridable for tests/conformance via
    /// `UNPEEL_SERVICE_MANAGER=launchd|systemd`.
    pub fn detect() -> Result<Self, String> {
        if let Ok(value) = std::env::var("UNPEEL_SERVICE_MANAGER") {
            return match value.trim() {
                "launchd" => Ok(Self::Launchd),
                "systemd" => Ok(Self::Systemd),
                other => Err(format!("unknown UNPEEL_SERVICE_MANAGER {other:?}")),
            };
        }
        if cfg!(target_os = "macos") {
            Ok(Self::Launchd)
        } else {
            Ok(Self::Systemd)
        }
    }
}

/// What the unit runs: the whole machine service, or one scoped workspace.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ServiceScope {
    Machine,
    Workspace { slug: String, home: PathBuf },
}

impl ServiceScope {
    fn validate(&self) -> Result<(), String> {
        if let Self::Workspace { slug, .. } = self {
            let valid = !slug.is_empty()
                && slug
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
            if !valid {
                return Err(format!(
                    "workspace {slug:?} is not a valid service unit name"
                ));
            }
        }
        Ok(())
    }

    fn launchd_label(&self) -> String {
        match self {
            Self::Machine => LAUNCHD_LABEL.into(),
            Self::Workspace { slug, .. } => format!("{LAUNCHD_LABEL}.{slug}"),
        }
    }

    fn systemd_unit(&self) -> String {
        match self {
            Self::Machine => format!("{SYSTEMD_UNIT}.service"),
            Self::Workspace { slug, .. } => format!("{SYSTEMD_UNIT}-{slug}.service"),
        }
    }
}

fn home_dir() -> Result<PathBuf, String> {
    std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| "HOME is not set".into())
}

fn unit_path(manager: ServiceManager, scope: &ServiceScope) -> Result<PathBuf, String> {
    let home = home_dir()?;
    Ok(match manager {
        ServiceManager::Launchd => home
            .join("Library")
            .join("LaunchAgents")
            .join(format!("{}.plist", scope.launchd_label())),
        ServiceManager::Systemd => {
            let config = std::env::var_os("XDG_CONFIG_HOME")
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".config"));
            config
                .join("systemd")
                .join("user")
                .join(scope.systemd_unit())
        }
    })
}

fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Render a unit from its checked-in template. The templates document these
/// exact anchor lines as the ones install rewrites. `graphical` selects the
/// desktop-session-bound systemd template (Linux only; see `install`).
pub fn render_unit(
    manager: ServiceManager,
    scope: &ServiceScope,
    binary: &Path,
    graphical: bool,
) -> String {
    let binary = binary.display().to_string();
    match manager {
        ServiceManager::Launchd => {
            let mut unit = LAUNCHD_TEMPLATE.replace(
                &format!("<string>{TEMPLATE_BINARY}</string>"),
                &format!("<string>{}</string>", xml_escape(&binary)),
            );
            if let ServiceScope::Workspace { slug, .. } = scope {
                unit = unit
                    .replace(
                        &format!("<string>{LAUNCHD_LABEL}</string>"),
                        &format!("<string>{}</string>", scope.launchd_label()),
                    )
                    .replace(
                        "\t\t<string>serve</string>",
                        &format!(
                            "\t\t<string>--workspace</string>\n\t\t<string>{}</string>\n\t\t<string>serve</string>",
                            xml_escape(slug)
                        ),
                    );
            }
            unit
        }
        ServiceManager::Systemd => {
            let exec = match scope {
                ServiceScope::Machine => format!("ExecStart={binary} serve"),
                ServiceScope::Workspace { slug, .. } => {
                    format!("ExecStart={binary} --workspace {slug} serve")
                }
            };
            let template = if graphical {
                SYSTEMD_GRAPHICAL_TEMPLATE
            } else {
                SYSTEMD_TEMPLATE
            };
            let mut unit = template.replace(&format!("ExecStart={TEMPLATE_BINARY} serve"), &exec);
            if let ServiceScope::Workspace { slug, .. } = scope {
                let description = if graphical {
                    format!("Description=Unpeel Host service (workspace {slug}, desktop session)")
                } else {
                    format!("Description=Unpeel Host service (workspace {slug})")
                };
                unit = unit
                    .replace(
                        "Description=Unpeel Host service (desktop session)",
                        &description,
                    )
                    .replace(
                        "Description=Unpeel Host service\n",
                        &format!("{description}\n"),
                    );
            }
            unit
        }
    }
}

fn run_tool(name: &str, args: &[String]) -> Result<(bool, String), String> {
    let output = Command::new(name)
        .args(args)
        .output()
        .map_err(|error| format!("could not run {name}: {error}"))?;
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    Ok((output.status.success(), text.trim().to_string()))
}

fn require_tool(name: &str, args: &[&str]) -> Result<(), String> {
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    let (ok, text) = run_tool(name, &args)?;
    if ok {
        Ok(())
    } else {
        Err(format!("{name} {} failed: {text}", args.join(" ")))
    }
}

fn gui_domain() -> String {
    format!("gui/{}", unsafe { libc::getuid() })
}

fn write_unit(path: &Path, body: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("prepare {}: {error}", parent.display()))?;
    }
    let temporary = path.with_extension("tmp");
    std::fs::write(&temporary, body).map_err(|error| error.to_string())?;
    std::fs::rename(&temporary, path).map_err(|error| error.to_string())
}

/// Write the unit for the resolved binary, then enable + start it. Re-runs
/// rewrite and restart the same unit (idempotent). `graphical` writes the
/// desktop-session-bound variant instead (systemd only): it is enabled but
/// not restarted here, because systemd starts it with
/// `graphical-session.target` — starting it from a shell with no display
/// would only make the Host advertise "no graphical session".
pub fn install(
    manager: ServiceManager,
    scope: &ServiceScope,
    binary: &Path,
    graphical: bool,
) -> Result<PathBuf, String> {
    scope.validate()?;
    if graphical && manager != ServiceManager::Systemd {
        return Err(
            "--graphical is a Linux (systemd --user) option: on macOS the Unpeel app owns the \
desktop-session daemon"
                .into(),
        );
    }
    let path = unit_path(manager, scope)?;
    write_unit(&path, &render_unit(manager, scope, binary, graphical))?;
    match manager {
        ServiceManager::Launchd => {
            let target = format!("{}/{}", gui_domain(), scope.launchd_label());
            // A previous generation may be loaded; unload it first so
            // bootstrap re-reads the rewritten plist. "not loaded" is fine.
            let _ = run_tool("launchctl", &["bootout".into(), target.clone()]);
            require_tool(
                "launchctl",
                &["bootstrap", &gui_domain(), &path.display().to_string()],
            )?;
            let _ = run_tool("launchctl", &["enable".into(), target]);
        }
        ServiceManager::Systemd => {
            let unit = scope.systemd_unit();
            // A graphical unit starts now only if the desktop session is
            // already up; otherwise the target's activation starts it. Ask
            // before enabling: the target's state is independent of ours.
            let start_now = if graphical {
                run_tool(
                    "systemctl",
                    &[
                        "--user".into(),
                        "is-active".into(),
                        "graphical-session.target".into(),
                    ],
                )?
                .0
            } else {
                true
            };
            require_tool("systemctl", &["--user", "daemon-reload"])?;
            require_tool("systemctl", &["--user", "enable", &unit])?;
            if start_now {
                require_tool("systemctl", &["--user", "restart", &unit])?;
            }
        }
    }
    Ok(path)
}

/// Stop the managed service and remove its unit file. Workspace data and
/// running Session hosts are deliberately untouched.
pub fn uninstall(manager: ServiceManager, scope: &ServiceScope) -> Result<PathBuf, String> {
    scope.validate()?;
    let path = unit_path(manager, scope)?;
    match manager {
        ServiceManager::Launchd => {
            // "not loaded" is success for an idempotent uninstall.
            let _ = run_tool(
                "launchctl",
                &[
                    "bootout".into(),
                    format!("{}/{}", gui_domain(), scope.launchd_label()),
                ],
            );
        }
        ServiceManager::Systemd => {
            let unit = scope.systemd_unit();
            let _ = run_tool(
                "systemctl",
                &["--user".into(), "disable".into(), "--now".into(), unit],
            );
        }
    }
    match std::fs::remove_file(&path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("remove {}: {error}", path.display())),
    }
    if manager == ServiceManager::Systemd {
        let _ = run_tool("systemctl", &["--user".into(), "daemon-reload".into()]);
    }
    Ok(path)
}

pub struct ServiceStatusReport {
    pub unit_path: PathBuf,
    pub unit_installed: bool,
    pub manager_state: String,
    /// The existing supervisor/worker lease truth, independent of the
    /// service manager's opinion.
    pub serve_running: bool,
    /// The installed unit is the desktop-session-bound variant.
    pub graphical_unit: bool,
    /// systemd only: `systemctl --user is-active graphical-session.target`.
    pub graphical_target_state: Option<String>,
    /// The desktop session visible to *this* process (display + session
    /// bus), or why there is none — the same check the Host publishes as
    /// computerUseAvailable.
    pub desktop_session: Result<String, String>,
}

pub fn status(
    manager: ServiceManager,
    scope: &ServiceScope,
) -> Result<ServiceStatusReport, String> {
    scope.validate()?;
    let path = unit_path(manager, scope)?;
    let manager_state = match manager {
        ServiceManager::Launchd => {
            let (ok, _) = run_tool(
                "launchctl",
                &[
                    "print".into(),
                    format!("{}/{}", gui_domain(), scope.launchd_label()),
                ],
            )?;
            if ok {
                "loaded".into()
            } else {
                "not loaded".into()
            }
        }
        ServiceManager::Systemd => {
            let (_, text) = run_tool(
                "systemctl",
                &["--user".into(), "is-active".into(), scope.systemd_unit()],
            )?;
            if text.is_empty() {
                "unknown".into()
            } else {
                text
            }
        }
    };
    let serve_running = match scope {
        ServiceScope::Machine => crate::service::is_running(),
        ServiceScope::Workspace { home, .. } => crate::driver::is_running_at(home),
    };
    let graphical_unit = std::fs::read_to_string(&path)
        .map(|body| body.contains(GRAPHICAL_MARKER))
        .unwrap_or(false);
    let graphical_target_state = match manager {
        ServiceManager::Systemd => {
            let (_, text) = run_tool(
                "systemctl",
                &[
                    "--user".into(),
                    "is-active".into(),
                    "graphical-session.target".into(),
                ],
            )?;
            Some(if text.is_empty() {
                "unknown".into()
            } else {
                text
            })
        }
        ServiceManager::Launchd => None,
    };
    let desktop_session =
        unpeel_core::computer_engine::desktop_session().map(|session| session.display);
    Ok(ServiceStatusReport {
        unit_installed: path.exists(),
        unit_path: path,
        manager_state,
        serve_running,
        graphical_unit,
        graphical_target_state,
        desktop_session,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace() -> ServiceScope {
        ServiceScope::Workspace {
            slug: "teama".into(),
            home: PathBuf::from("/tmp/x"),
        }
    }

    #[test]
    fn launchd_machine_unit_rewrites_only_the_binary() {
        let unit = render_unit(
            ServiceManager::Launchd,
            &ServiceScope::Machine,
            Path::new("/opt/unpeel/bin/unpeel"),
            false,
        );
        assert!(unit.contains("<string>com.unpeel.serve</string>"));
        assert!(unit.contains("<string>/opt/unpeel/bin/unpeel</string>"));
        assert!(unit.contains("<string>serve</string>"));
        assert!(!unit.contains(&format!("<string>{TEMPLATE_BINARY}</string>")));
        assert!(!unit.contains("<string>--workspace</string>"));
    }

    #[test]
    fn launchd_workspace_unit_scopes_label_and_arguments() {
        let unit = render_unit(
            ServiceManager::Launchd,
            &workspace(),
            Path::new("/opt/unpeel/bin/unpeel"),
            false,
        );
        assert!(unit.contains("<string>com.unpeel.serve.teama</string>"));
        assert!(unit.contains("<string>--workspace</string>"));
        assert!(unit.contains("<string>teama</string>"));
        assert!(unit.contains("<string>serve</string>"));
    }

    #[test]
    fn systemd_units_render_exec_start() {
        let machine = render_unit(
            ServiceManager::Systemd,
            &ServiceScope::Machine,
            Path::new("/home/u/.local/bin/unpeel"),
            false,
        );
        assert!(machine.contains("ExecStart=/home/u/.local/bin/unpeel serve"));
        let scoped = render_unit(
            ServiceManager::Systemd,
            &workspace(),
            Path::new("/home/u/.local/bin/unpeel"),
            false,
        );
        assert!(scoped.contains("ExecStart=/home/u/.local/bin/unpeel --workspace teama serve"));
        assert!(scoped.contains("(workspace teama)"));
    }

    #[test]
    fn graphical_units_bind_to_the_desktop_session_target() {
        let machine = render_unit(
            ServiceManager::Systemd,
            &ServiceScope::Machine,
            Path::new("/usr/local/bin/unpeel"),
            true,
        );
        assert!(machine.contains("ExecStart=/usr/local/bin/unpeel serve"));
        assert!(machine.contains(GRAPHICAL_MARKER));
        assert!(machine.contains("After=graphical-session.target"));
        assert!(machine.contains("WantedBy=graphical-session.target"));
        assert!(!machine.contains("WantedBy=default.target"));
        assert!(machine.contains("Description=Unpeel Host service (desktop session)"));
        let scoped = render_unit(
            ServiceManager::Systemd,
            &workspace(),
            Path::new("/usr/local/bin/unpeel"),
            true,
        );
        assert!(scoped.contains("ExecStart=/usr/local/bin/unpeel --workspace teama serve"));
        assert!(scoped.contains("(workspace teama, desktop session)"));
        assert!(scoped.contains(GRAPHICAL_MARKER));
        // The plain unit never carries the marker status keys off.
        let plain = render_unit(
            ServiceManager::Systemd,
            &ServiceScope::Machine,
            Path::new("/usr/local/bin/unpeel"),
            false,
        );
        assert!(!plain.contains(GRAPHICAL_MARKER));
        assert!(plain.contains("WantedBy=default.target"));
    }

    #[test]
    fn unit_names_reject_unsafe_workspace_slugs() {
        for bad in ["", "Bad", "a b", "a/b", "a.b"] {
            let scope = ServiceScope::Workspace {
                slug: bad.into(),
                home: PathBuf::from("/tmp/x"),
            };
            assert!(scope.validate().is_err(), "{bad:?} should be rejected");
        }
        assert!(workspace().validate().is_ok());
    }

    #[test]
    fn unit_file_names_are_scoped() {
        assert_eq!(ServiceScope::Machine.systemd_unit(), "unpeel-serve.service");
        assert_eq!(workspace().systemd_unit(), "unpeel-serve-teama.service");
        assert_eq!(workspace().launchd_label(), "com.unpeel.serve.teama");
    }
}
