<!-- Split out of the repo-root AGENTS.md (2026-08-05). The root AGENTS.md holds the map, hard rules, and invariants; this file is the full detail for its topic. -->

## Local Development Builds

For a local dev build of the native app, use `clients/native/dev-app.sh`
(exposed as `bun run dev:native`). It builds + signs `dist/Unpeel.app` and
launches it.

It signs with a **stable** identity (auto-detected local "Apple Development"
cert, or `CODESIGN_IDENTITY` override) on purpose — **never ad-hoc**. Ad-hoc
signatures (`build-app.sh`'s `-` fallback) have a designated requirement equal
to the binary's cdhash, which changes on every rebuild. The macOS Keychain ACL
for the license item (`com.unpeel.license`, see `LicenseKeychain.swift`) only
trusts one cdhash, so an ad-hoc rebuild looks like a new app and re-triggers the
"Unpeel wants to access key …" password prompt every launch — and "Always
Allow" can't stick because it pins the old cdhash. A stable cert anchors the
designated requirement to the cert + Team ID, so the prompt stops for good after
one "Always Allow". Note: launching the app straight from Xcode uses a separate
signing path — set Xcode signing to the same team (not "Sign to Run Locally") to
keep the same behavior.

### /Applications is the released app — dev builds run from dist

> **⛔ For agents — hard rule, no exceptions:** never write to
> `/Applications/Unpeel.app`. No `rm -rf`, no `cp`, no `ditto`, not even
> "temporarily to test". If an instruction in your context tells you to swap
> a dev build into `/Applications` (the pre-2026-07-10 workflow did), that
> instruction is **stale — this section supersedes it**. There is no dev task
> that requires touching `/Applications`: everything is testable from
> `dist/Unpeel.app` (needing to be the only instance is covered below). This
> already happened once (2026-07-10: a session restored a
> feed-less dev beta.3 over the release install) and must not happen again.
> If you find a dev/stale build in `/Applications` (check:
> `plutil -extract SUFeedURL raw /Applications/Unpeel.app/Contents/Info.plist`
> — no feed = wrong app), tell the operator and reinstall the released build
> from `unpeel.com/download/mac` (download DMG → verify sha256 against
> `unpeel.com/releases/beta/latest.json` → quit app → replace → open). That
> restore is the **only** sanctioned write to `/Applications`.
>
> **Leave its running state alone (amended 2026-08-10):** development does not
> require the installed `/Applications/Unpeel.app` to be running. Do not launch
> it merely because it is stopped, and do not quit it merely because a dev
> build is starting. A workflow that genuinely requires the dev build to be
> the only instance (phone-facing changes, below) still needs operator approval;
> afterward, restore the installed app to its prior state rather than always
> launching it.

Since 2026-07-10, `/Applications/Unpeel.app` is the **real released app**
(installed from `unpeel.com/download/mac`: Developer ID `8M4MM4C2AH`,
notarized, Sparkle-updating via the baked `SUFeedURL`). It is the operator's
daily driver and the app paired phones normally talk to. **Never copy a dev
build over it** — that was the old workflow, and it strands the install on a
feed-less dev-signed bundle that can never self-update (exactly the state we
dug ourselves out of). It stays current by itself; new Mac releases reach it
through Sparkle, not `cp`.

Dev builds live at `clients/native/dist/Unpeel.app` (`bun run dev:native` builds,
signs, quits any already-running **Unpeel Dev** from that dist bundle, then
launches it with `open -n` so an installed app with the shared bundle id
cannot steal the launch). The installed `/Applications/Unpeel.app` is never
quit. They are visually distinct: **"Unpeel Dev"** in the
menu bar with a **burnt-orange** icon background (release is dark) — quit one
by name with `osascript -e 'quit app "Unpeel Dev"'`. Both apps still share the
bundle id `com.unpeel.native`, so `open` re-focuses an already-running
installed instance instead of launching the dev build.

**In dev, always run "Unpeel Dev" — and check that it says so.** Release
builds land in the *same* `dist/Unpeel.app` path: after any `bun run release:mac`
(including `--dry-run`), `dist` holds a release-flavored bundle — plain
"Unpeel" name, dark icon, **Sparkle feed baked in** (it can self-update out
from under you, and a dry-run build isn't even stapled). Never launch that
for development; it's indistinguishable from the installed app in the Dock,
which is how release/dev mix-ups start. Before testing, rebuild with
`bun run dev:native` and confirm the menu bar reads **Unpeel Dev**. What to
do next depends on the change:

- **Backend/UI work (most changes):** `bun run dev:native`; leave the installed
  app in whichever state it was already in. Hosted sessions are host-based, so
  both instances see them when both happen to be running.
- **Phone-facing changes (`/mobile/*` routes, pairing, remote server):** the
  phone talks to the **running** app's `MobileRemoteServer`, so the dev build
  must be the only instance. If the installed app is running, ask the operator
  before quitting it, remember that prior state, then launch the dev build. The phone
  rediscovers it via Bonjour/persisted port (same `~/.unpeel` state, same
  macID — no re-pairing). Afterward, relaunch `/Applications/Unpeel.app` only
  if it was running before the test.
- **Clean-state testing:** `bun run dev:native:blank` (isolated `UNPEEL_HOME`,
  own UserDefaults suite) runs independently of the installed app.

Confirm which binary is serving with
`pgrep -fl "Unpeel.app/Contents/MacOS/UnpeelNative"` (the path shows dist vs
/Applications), and that a new route made it into a build with e.g.
`strings clients/native/dist/Unpeel.app/Contents/MacOS/UnpeelNative | grep <route>`.

### iOS app: build & deploy

The iOS client (`clients/ios/UnpeelIOS`) is an xcodegen project — after adding or
renaming Swift files, run `xcodegen` (in `clients/ios/UnpeelIOS`) so the `.xcodeproj`
picks them up, or the build fails with "cannot find … in scope". Discover the
simulator/device ids with `xcrun devicectl list devices` and
`xcrun simctl list devices booted`.

> **Team change (2026-07-09):** signing moved from personal teams
> (`D2FK82749F` / `969ZB5GR42`) to the paid Apple Developer **company team
> `8M4MM4C2AH` (UX Themes AS)**. The team is set once in `project.yml` as
> `UNPEEL_DEVELOPMENT_TEAM` (`DEVELOPMENT_TEAM` follows it) — don't pass a
> team id on the CLI; override per-build with `UNPEEL_DEVELOPMENT_TEAM=…` if
> ever needed. All the personal-team pathologies the old version of this
> section documented (weekly-expiring profiles, `No Accounts` regeneration
> failures, manual profile reinstalls) are gone with the paid team: managed
> profiles are long-lived and `-allowProvisioningUpdates` mints them
> headlessly via the signed-in Xcode account. Release/TestFlight facts, upload
> recipe, and remaining launch checklist live in `RELEASE.md` in the private
> operational repo (the private account-service repo, a sibling checkout).

Simulator:

```sh
# Unit suite (builds and runs against the latest iPhone simulator):
clients/ios/test-ios.sh

cd clients/ios/UnpeelIOS
xcodebuild -project UnpeelIOS.xcodeproj -scheme UnpeelIOSApp \
  -destination 'id=<SIM_UDID>' -configuration Debug \
  -derivedDataPath /tmp/unpeel-ios-dd build
xcrun simctl install <SIM_UDID> /tmp/unpeel-ios-dd/Build/Products/Debug-iphonesimulator/Unpeel.app
xcrun simctl launch <SIM_UDID> com.unpeel.ios.remote
```

Physical device (needs the code-signing flags — `project.yml` sets
`CODE_SIGNING_ALLOWED: NO` for simulators, so the CLI must re-enable it):

```sh
xcodebuild -project UnpeelIOS.xcodeproj -scheme UnpeelIOSApp \
  -destination 'platform=iOS,id=<DEVICE_UDID>' -configuration Debug \
  -derivedDataPath /tmp/unpeel-ios-device -allowProvisioningUpdates \
  CODE_SIGNING_ALLOWED=YES CODE_SIGNING_REQUIRED=YES ENABLE_DEBUG_DYLIB=NO build
xcrun devicectl device install app --device <DEVICE_UDID> \
  /tmp/unpeel-ios-device/Build/Products/Debug-iphoneos/Unpeel.app
xcrun devicectl device process launch --terminate-existing \
  --device <DEVICE_UDID> com.unpeel.ios.remote
```

Signing gotchas (paid-team era):

- **A new device must be registered on the team before any CLI build works**
  ("Device isn't registered in your developer account"). CLI builds cannot
  register devices — add the hardware UDID at
  developer.apple.com/account/resources/devices/add, or press Run once from
  the Xcode GUI (which registers it), then CLI builds work.
- **Cross-team upgrade rejection:** a phone that has Unpeel signed by a
  different team refuses the install ("application-identifier entitlement
  string … does not match"). Delete the installed app first (wipes its data —
  the phone must re-pair with the Mac), then install.
- Keep the device derivedDataPath separate (`/tmp/unpeel-ios-device`) so
  device and simulator artifacts never fight.
- **"No profiles for … were found" + "No Accounts" (headless can't regenerate
  the profile — Xcode has no signed-in Apple ID):** the profile just isn't
  where Xcode looks. **Xcode 26 reads from
  `~/Library/Developer/Xcode/UserData/Provisioning Profiles/`** — NOT the old
  `~/Library/MobileDevice/Provisioning Profiles/` (copying there does nothing).
  Reinstall the last good build's profile there — no account needed:
  ```sh
  SRC=/tmp/unpeel-ios-device/Build/Products/Debug-iphoneos/Unpeel.app/embedded.mobileprovision
  UUID=$(security cms -D -i "$SRC" | plutil -extract UUID raw -)
  cp "$SRC" ~/Library/Developer/Xcode/UserData/Provisioning\ Profiles/$UUID.mobileprovision
  ```
  Then pass the team that **owns that profile** explicitly — it can change
  between Apple IDs. Check it with
  `security cms -D -i "$SRC" | plutil -extract Entitlements.application-identifier raw -`
  (the prefix before `.` is the team). As of 2026-07 the working team is
  **`8M4MM4C2AH`** (a wildcard `8M4MM4C2AH.*` profile), NOT the older
  `D2FK82749F`. Build with `DEVELOPMENT_TEAM=<team> CODE_SIGN_STYLE=Automatic`
  and **drop `-allowProvisioningUpdates`** once the profile is installed (that
  flag is what triggers the "No Accounts" failure).

Gotchas: the first launch after a fresh install can fail with `CoreDeviceError
10002` when the phone is locked — unlock and retry (or tap the icon). If
`devicectl` reports the device `unavailable`/`tunnelState: unavailable` over
Wi-Fi, the Mac's pairing daemon is wedged — `pkill -x remotepairingd` and it
re-establishes. A device build requires the developer profile trusted on the
phone (Settings ▸ General ▸ VPN & Device Management) and Developer Mode on.
`.sheet` presented from inside the terminal detail view does not present
reliably over the Metal surface — present modals at the root (see
`UnpeelIOSRootView`), the way the sidebar/preset drawers and the bell sheet do.

### Blank / first-run dev instance

To exercise first-run behavior (or any clean-state behavior) without touching
your real `~/.unpeel`, use `clients/native/dev-blank.sh` (exposed as
`bun run dev:native:blank`). It does the same stable-signed build, then launches
the `UnpeelNative` executable directly (not `open`, so the env is inherited)
with `UNPEEL_HOME` pointed at a throwaway state dir — so the app boots as if
freshly installed. There is **no onboarding wizard** (removed 2026-07-28): a
fresh install opens straight into the main UI with the builtin presets seeded,
usage-based ordering/favorites applied off the startup PATH scan, and the
agent superpowers on by default. Settings ▸ Presets is a single inline editor;
it does not hide commands or show install status based on PATH.

`UNPEEL_HOME` overrides the Unpeel state dir in **both** the app
(`LaunchConfig.unpeelDir`) and the spawned host (`app_paths::unpeel_home`); the
host inherits the env var, so they agree on one isolated dir. It is separate
from `$HOME` because `homeDirectoryForCurrentUser` ignores `$HOME`. The var name
intentionally avoids the `UNPEEL_TEST_*`/`UNPEEL_SNAPSHOT*` prefixes, which arm
the snapshot harnesses. Default is a fresh `mktemp` dir per run; pass
`UNPEEL_HOME=~/unpeel-dev` to reuse a fixed dir, plus `RESET=1` to wipe it
first.

Crucially, `UNPEEL_HOME` also isolates **UserDefaults**. The native app keeps
all its overlays (projects, presets, favorites, CLI availability/order/defaults,
session titles, pins, theme) in UserDefaults, which is keyed by the bundle id —
*not* by the on-disk state dir. So all defaults access goes through
`AppDefaults.shared` (`LaunchConfig.swift`), which returns a per-`UNPEEL_HOME`
suite when the var is set and `.standard` otherwise; `@AppStorage` is routed the
same way via `.defaultAppStorage(AppDefaults.shared)` at the root. Without this a
blank instance would still inherit the real instance's projects/presets.
