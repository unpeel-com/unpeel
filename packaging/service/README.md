# Unpeel Host service units

Start `unpeel serve` (the UI-free Unpeel Host service) on boot, per user.
These files are the templates `unpeel serve install` renders; they are also
usable verbatim in a container or golden image when `unpeel` is installed at
`/usr/local/bin/unpeel` (what `curl -fsSL https://unpeel.com/install.sh | sh`
does).

The easy path on the Host machine itself:

```sh
unpeel serve install     # write the unit, enable it, start it
unpeel serve status      # unit state + live Host service status
unpeel serve uninstall   # stop the service and remove the unit only
```

`unpeel serve install` resolves the running `unpeel` binary's real path into
the unit. With no `UNPEEL_HOME` it installs the machine service (one
supervisor, every registered workspace). With `--workspace NAME` (or an
`UNPEEL_HOME` that is a registered workspace) it installs a scoped
single-workspace unit — the container/explicit-unit shape. `uninstall` stops
the service and deletes the unit file; it never touches `~/.unpeel` data, and
running Session PTYs survive a service stop by design.

## macOS — per-user LaunchAgent (`com.unpeel.serve.plist`)

Written to `~/Library/LaunchAgents/com.unpeel.serve.plist` (scoped:
`com.unpeel.serve.<workspace>.plist`).

This must stay a **per-user LaunchAgent**, never a root LaunchDaemon: the
service owns `~/.unpeel`, the user Keychain, and the per-user machine lease.
Consequence for a headless Mac: enable **automatic login** for the hosting
user (System Settings ▸ Users & Groups) so the `gui/<uid>` launchd domain
exists after a reboot with no one at the keyboard.

Manual verbatim use:

```sh
cp com.unpeel.serve.plist ~/Library/LaunchAgents/
launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/com.unpeel.serve.plist
```

## Linux — systemd user unit (`unpeel-serve.service`)

Written to `~/.config/systemd/user/unpeel-serve.service` (scoped:
`unpeel-serve-<workspace>.service`). `unpeel-serve@.service` is the manual
template-instance spelling of the scoped shape.

This is deliberately a `--user` unit for the same ownership reasons. For a
headless box the user manager must outlive login sessions:

```sh
sudo loginctl enable-linger <user>
```

Manual verbatim use:

```sh
cp unpeel-serve.service ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now unpeel-serve.service
```

## Linux — desktop-session variant (`unpeel-serve-graphical.service`)

Computer Use (the private "computer-use-release" design record) needs the Host inside
the desktop session: the engine reads the session's `DISPLAY` /
`WAYLAND_DISPLAY` and the accessibility (AT-SPI) bus on the session D-Bus.
`unpeel serve install --graphical` writes this template instead of the
plain one (same file name, `unpeel-serve.service`; scoped:
`unpeel-serve-<workspace>.service`): it is `PartOf=` / `WantedBy=`
`graphical-session.target`, so it starts when the desktop session activates
and stops when it ends, inheriting the display the session manager imported
into the user manager. GNOME, KDE, and sway do that import and pull the
target in from their own session target. A hand-rolled session (an Xvfb
script, a streamed Xorg desktop such as a Box) has no session manager, and
`graphical-session.target` refuses manual start by design, so it uses the
checked-in `unpeel-desktop-session.target` (`BindsTo=graphical-session.target`)
once its display is up:

```sh
cp unpeel-desktop-session.target ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user import-environment DISPLAY XAUTHORITY
systemctl --user start unpeel-desktop-session.target     # stop it when the session ends
```

An `ExecStartPre=` import inside the unit cannot substitute for that: it
runs with the user manager's environment, which is exactly what lacks the
display. `unpeel serve status` prints the variant,
`graphical-session.target`'s state, and the desktop session (display plus
session bus) visible to the calling shell.

## Diagnostics

Service stdout is not the diagnostic surface (launchd discards it; systemd
journals it). Durable diagnostics: `~/.unpeel/hooks/trace.log`, plus
`~/.unpeel/host-service.json` (machine) and `<home>/serve.json` (workspace).
