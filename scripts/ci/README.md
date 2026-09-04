# Computer-use CI / proof fixtures

Reusable pieces behind the Computer Use 0.5.0 proof
(`unpeel-apple:docs/plans/computer-use-release.md`, Lanes C and E). No secrets, no real
`~/.unpeel`.

- **`fake-cua-driver.sh`** — a stand-in for `cua-driver`: `serve`/`status`/
  `stop` on a real UNIX socket, and `call get_window_state` (fixed
  non-degraded tree, plus a 1×1 PNG when `--screenshot-out-file` is given —
  that is how `see` captures) / `call screenshot` (the PNG alone). Point a
  Host at it with `UNPEEL_CUA_DRIVER_BIN`; `FAKE_CUA_DRIVER_LOG=<file>`
  records every invocation so a test can prove which engine calls a Host
  made (`call end_session` from `__computer_cleanup__` on Remove). Lets a
  Linux `unpeel serve` advertise computer use without the real engine; the
  matrix case `crates/unpeel-cli/tests/cases/computer.py` is built on it.
  The REAL-engine proof is `scripts/verify-computer.sh` (Xvfb + Openbox +
  zenity, pinned cua-driver).
- **`computer-use-linux-host.sh`** — runs `unpeel serve` on a Linux box or
  container as a computer-use-capable Host: an Xvfb display plus the stub
  above, and `computer_use`/`computer_access` turned on. This is the
  "fake Host bootstrap fixture (stub driver)".

Bring-up used for the Lane C proof (OrbStack VM `unpeel-clis`, arm64 Ubuntu):

```sh
# in the VM, from this branch's checkout mounted at /Users/.../cu-lane-c:
curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain 1.88.0
sudo apt-get install -y xvfb build-essential pkg-config libssl-dev
CARGO_TARGET_DIR=$HOME/cu-target cargo build -p unpeel-cli -p unpeel-host \
  --manifest-path /path/to/cu-lane-c/crates/Cargo.toml
UNPEEL_BIN_DIR=$HOME/cu-target/debug \
  /path/to/cu-lane-c/scripts/ci/computer-use-linux-host.sh ~/.unpeel-cu-proof
# pair a Controller from another shell:
UNPEEL_HOME=~/.unpeel-cu-proof unpeel pair
```

The operator-run Mac click-through scripts (stage a release-flavored bundle,
launch it against an isolated home with the VM's pairing code, tear it down)
live in a scratch dir, not the repo, because they hardcode a bundle path and
a LAN address; see the Lane C report.
