<!-- Split out of the repo-root AGENTS.md (2026-08-05). The root AGENTS.md holds the map, hard rules, and invariants; this file is the full detail for its topic. -->

## Creating a Release

A release is cut from a Mac with **one command** — `clients/native/release.sh`
(exposed as `bun run release:mac`). It chains the existing per-step scripts;
there is no website/admin path, because signing, notarization, and Sparkle
signing need local secrets a Cloudflare Worker cannot hold. The release order
is CLI (`bun run release:cli`) → Mac app (`bun run release:mac`) → website
(the changelog entry goes live from the separate `unpeel-website` repo).

```sh
CODESIGN_IDENTITY="Developer ID Application: <team> (<TEAMID>)" \
NOTARY_KEY_PATH=~/.appstoreconnect/private_keys/AuthKey_<KEYID>.p8 \
NOTARY_KEY_ID=<KEYID> NOTARY_ISSUER=<issuer-uuid> \
bun run release:mac -- --channel beta --build 9
```

**Lockstep versioning (decided 2026-08-13):** the app and the `unpeel` CLI
share one version number, sourced from `crates/Cargo.toml`
(`[workspace.package] version`). Both `release.sh` and `release-cli.mjs`
derive it from there; passing `--version` is optional and both refuse a value
that differs from the workspace. To release a new version, bump
`crates/Cargo.toml`, run `cargo update --workspace`, and add the matching
changelog entry — every release event cuts both sides at the same number,
even when one barely changed.

First real release (`0.1.0-beta.6` build 8) shipped 2026-07-09 on the paid
team. Prefer the `NOTARY_KEY_*` ASC-API-key trio: `NOTARY_KEYCHAIN_PROFILE`
lives in the data-protection keychain, which silently stops resolving from
non-UI sessions. The current channel/build ledger and exact key ids live in
`RELEASE.md` in the private operational repo (the private account-service repo, the
sibling checkout) — CFBundleVersion is monotonic across channels, so
always check it before picking `--build`.

The pipeline (each step reuses an existing script):

1. `build-app.sh` — build + Developer ID sign `Unpeel.app` (hardened runtime);
   `--channel` bakes the matching `SUFeedURL` into Info.plist.
2. Notarize + staple the **app** (submit a throwaway ZIP via
   `notarize-dmg.sh <zip> --staple Unpeel.app`) — before packaging, so the app
   inside the DMG and inside the Sparkle ZIP both carry a stapled ticket and
   pass Gatekeeper offline.
3. `make-dmg.sh` — package + sign the install DMG (`Unpeel-<version>.dmg`)
   from the stapled app.
4. `notarize-dmg.sh` — notarize + staple the DMG itself.
5. `ditto` — zip the stapled app as the Sparkle self-update archive, into a
   cleaned per-channel staging dir (one ZIP → one appcast item; stale
   ZIPs/deltas are never re-advertised).
6. `generate_appcast` (Sparkle CLI, EdDSA key from the login Keychain) — sign the
   ZIP and write `appcast.xml` with URLs under `https://unpeel.com/releases/<channel>/`.
7. `scripts/publish-cloudflare-release.mjs` — upload DMG + ZIP + appcast +
   `latest.json` to R2.

Preflight refuses to run when: the checkout is not clean `main` at both the
local and live remote `origin/main`; the signing identity / Sparkle EdDSA key / wrangler auth
are missing; any channel manifest is unreachable, returns an
unexpected HTTP response, or is malformed; the versioned DMG/ZIP keys already
exist; `--build` is not greater than every channel's published build —
CFBundleVersion is one monotonic space across channels (`--force` overrides
the published-state guards; versioned artifacts are CDN-cached as immutable,
so overwriting a published version strands clients on a ZIP whose EdDSA
signature no longer matches the appcast) — or the version has no
`## <version>` entry in `unpeel-website:apps/website/app/changelog.md` (the website's
`/changelog` page; add the entry, and deploy the site after the release so it
goes live — dry runs are exempt). The lower-level publisher preserves validated
same-version fields for a partial/appcast repair; a new version must include
both its DMG and Sparkle ZIP so it cannot clobber `latest.json` with a partial
manifest. A same-version repair also cannot change the advertised build while
preserving old downloads; even with `--force`, that requires both replacement
artifacts.

Requirements: `CODESIGN_IDENTITY` (Developer ID Application), notary credentials
(the `NOTARY_KEY_PATH`/`NOTARY_KEY_ID`/`NOTARY_ISSUER` ASC-API-key trio
preferred; `NOTARY_KEYCHAIN_PROFILE` or the `NOTARY_APPLE_ID`/`NOTARY_TEAM_ID`/
`NOTARY_PASSWORD` trio also work), and the Sparkle EdDSA private key in the
login Keychain (from `generate_keys`). `--channel` drives both the compiled
feed URL and the upload target, so a build cannot point at the wrong appcast.

Flags: `--notes <file.html>` adds release notes shown in the Sparkle update
dialog (embedded as the appcast item `<description>`); `--dry-run` builds +
signs + appcasts locally but skips Apple notary and R2 upload (the app is
**not** stapled in this mode, and Sparkle artifacts stage under
`dist/sparkle-dryrun/` so they can never leak into a real appcast);
`--skip-notarize` for fast local iteration — it refuses to publish (combine
with `--dry-run`), since an un-notarized build must never reach R2. See
the private "cloudflare-releases" design record for the full walkthrough and the manual
fallbacks.

> **For agents:** you cannot cut a real release — it requires the operator's
> local secrets (Developer ID cert, notary credentials, Sparkle EdDSA key in the
> login Keychain) and Apple/R2 network calls. Validate changes to the release
> pipeline with `--dry-run` (a full local build + sign + appcast, no upload),
> then hand the real run to a human or a macOS CI runner with the secrets
> injected. `generate_appcast` is pinned to write `appcast.xml` via `-o`; without
> it the file is named after the feed URL (e.g. `appcast-beta.xml`).


## CLI (`unpeel`) Install Channel

The CLI installs with:

```sh
curl -fsSL https://unpeel.com/install.sh | sh
```

- `/install.sh` is served by the releases worker (`unpeel-website:apps/releases/src/worker.mjs`),
  which fetches `<channel>/cli/install.sh` from R2 per request (bounded
  60 s in-isolate cache, `served-assets.mjs`) and substitutes
  `__DEFAULT_CHANNEL__` / `__BASE_URL__` per request. The script's source is
  `scripts/install.sh` in this repo; `bun run release:cli` uploads it (plus
  `scripts/install-app.sh` and `protocol/app-registry.json`) per channel
  after `latest.json`, so an installer fix ships with the next CLI publish.
  The Worker keeps a deploy-time copy of all three as the fallback for a
  channel that has not published them yet or an R2 failure
  (`x-unpeel-asset-source: r2|fallback` on the response). Tests:
  `scripts/release-installer.test.mjs`, `scripts/release-worker-assets.test.mjs`.
- The installer detects the platform (`macos-universal`, `linux-x86_64`,
  `linux-aarch64` — same names as the vendored ghostty-vt slices), downloads
  `/releases/<channel>/cli/unpeel-latest-<target>.tar.gz` from the same R2
  bucket the app uses, requires and verifies the `.sha256` sidecar, and installs
  `unpeel` and `unpeel-host` (the CLI's `unpeel serve` and one-shot verbs
  spawn sessions via a sibling `unpeel-host` — `resolve_host_binary` in
  `session_ops.rs`) plus, when the
  archive carries it (0.4.5+), `unpeel-attach` (the Controller terminal
  client; harmless standalone, and what the Mac app will bundle from these
  archives after the repo split — the private "open-source" design record). The installer
  never requires `unpeel-attach`, so a worker deploy ahead of a CLI publish
  keeps installing the older two-binary `-latest` archive. Everything goes into
  `/usr/local/bin` if writable, else `~/.local/bin` (`UNPEEL_INSTALL_DIR`
  overrides; `UNPEEL_CHANNEL` picks alpha/beta/stable).
- Publishing coordinates: `scripts/r2.jsonc` (account id + bucket) and the
  root `wrangler` devDependency; neither publisher reads `unpeel-website:apps/website` or
  `unpeel-website:apps/releases` any more (`--bucket` / `UNPEEL_RELEASE_BUCKET` still
  override).
- Publishing: `bun run release:cli -- --channel beta` on a Mac builds both
  darwin triples (needs `rustup target add aarch64-apple-darwin
  x86_64-apple-darwin`), lipos them universal, ad-hoc re-signs, tars, and
  uploads versioned + `-latest` tarballs, sha256 sidecars, and
  `<channel>/cli/latest.json` via wrangler. Linux tarballs are built on a
  Linux box/CI with `scripts/build-cli-linux.sh` and attached with
  `--linux-x86_64 <tar.gz>` / `--linux-aarch64 <tar.gz>`. Every archive
  includes the three binaries (`unpeel`, `unpeel-host`, `unpeel-attach` —
  the last built from the standalone `crates/unpeel-attach` manifest and
  lipo'd the same way; `CLI_BINARIES` in `release-cli.mjs` is the one list),
  license notices covering all three crates, and `BUILD_PROVENANCE.json`;
  the publisher rejects a target/version/source commit mismatch, refuses an
  archive missing any of the three, and verifies that every binary header
  matches the advertised architecture. The "all three target archives from
  one commit" rule is unchanged: each target's archive must carry all three
  binaries from that same commit. `--dry-run`
  builds and prints the uploads
  without publishing. Versioned keys are immutable at the CDN — bump the
  version rather than `--force`.
- **Unpeel Apps** (design, usage, markdown) ship through one generalized
  lane: `curl -fsSL https://unpeel.com/install/<app>/install.sh | sh`,
  served from the single `scripts/install-app.sh` template (published per
  channel like `install.sh`) with
  the same substitutions, checksum-sidecar requirement, and install-dir
  rules; each installs the single `unpeel-<app>` binary and writes
  `~/.unpeel/<app>-install.json`. The app registry
  (`protocol/app-registry.json`, embedded by `unpeel-core` and published per
  channel by both `release:cli` and `release:app`) is the single source of truth:
  one entry there registers the app for both the worker's wildcard
  `/install/*` route (no per-app route patterns) and
  `scripts/release-app.mjs`. Publishing:
  `bun run release:app -- --app <app> --channel beta [--dry-run]` builds
  the macos-universal binary from the **sibling checkout
  `~/Dev/unpeel-app-<app>`** (design additionally path-depends on
  `~/Dev/unpeel-surface`), lipos/ad-hoc signs/tars it, and uploads
  versioned + `-latest` tarballs and sha256 sidecars under
  `<channel>/<app>/`. Linux tarballs attach with `--linux-*` like the CLI.
  Hosts may install the same assets directly with
  `unpeel apps install <unpeel.app.id>`; those managed copies live in
  `~/.unpeel/apps/bin`, which precedes ordinary PATH discovery.
  Interactive installs ask for confirmation and unattended user-owned
  automation must pass `--yes`. A Host advertises `apps.install` only on a
  platform for which the publisher defines a release target.
  Publishing and serving are deliberately two explicit steps. Simpler than
  release:cli by design: no latest.json manifest or provenance yet — add
  them when apps get an update check. `release:design` remains an alias
  for `release:app -- --app design`.
- A preannouncement recovery that must keep the semantic version can use
  `--artifact-revision "$(git rev-parse --short=12 HEAD)"` with all three
  target archives. The 12 lowercase hex characters must match the clean
  publish checkout's current HEAD; recovery mode rejects `--force`, partial
  target sets, a missing/different published semantic version, and any
  pre-existing revisioned archive or sidecar. It writes new immutable
  `unpeel-<version>-<revision>-<target>.tar.gz(.sha256)` objects, records the
  revision and sidecar locations in `cli/latest.json`, then replaces the
  mutable latest archive/checksum pairs. Every immutable archive and sidecar
  finishes before the first mutable alias is touched; the manifest remains
  last. Once a same-version manifest uses revisioned artifacts, another
  same-version publish must be a complete new revisioned recovery (or the
  semantic version must be bumped). Normal releases keep the legacy key and
  manifest shape.
- The CLI channel needs no Apple secrets (no notarization/Sparkle): agents can
  run the real build, but the R2 upload still needs the operator's wrangler
  auth. Same-version staged publishes merge existing targets into
  `latest.json`; a version bump starts a fresh manifest, so pass every target
  in the same command. The publisher rejects a first/new-version publish
  unless all three target archives are present. Manifest/network/HTTP errors
  fail closed; even `--force` may replace unread manifest state only when all
  three target archives are supplied, so a recovery cannot silently drop
  platforms.

Revision recovery does not make already-installed clients on the same
semantic version show an update toast. This is acceptable only before a
release is announced (or when affected users will reinstall). Without a CDN
purge, mutable latest archive/checksum aliases can also disagree for up to
their 300-second cache lifetime; the installer fails closed on that checksum
mismatch. Wait past that TTL and prove fresh unauthenticated installer bytes
on every platform before calling the recovery live.

On the Linux architecture being packaged, use
`scripts/build-cli-linux.sh` (or `bun run release:cli:linux` when Bun is
available). It builds both release binaries, creates the correctly named
tarball plus a SHA-256 sidecar under `dist/cli/`, embeds the source commit and
dirty-state provenance, runs the packaged `unpeel --version`, and prints the
exact `release:cli` attachment flag. Official archives have a hard GLIBC 2.31
ceiling (Ubuntu 20.04 / Debian 11); the build script inspects both binaries and
fails if a newer build host raises that floor. The x86 CI artifact is therefore
built inside the pinned Rust 1.88 Bullseye container and smoke-tested on Ubuntu
20.04 — never build an official archive directly on `ubuntu-latest`. A real
publish also requires clean
`main` aligned with both the local and live remote `origin/main`. Do not
label a cross-compiled archive as runtime-tested; run this on each advertised
architecture or in a matching CI runner.

### CLI update toast (removed 2026-09-03)

The now-removed interactive terminal UI used to check its install channel for
a newer published version and show a persistent, click-to-dismiss toast in
the top-right (same slot as the transient verb toast, which took precedence
while up); `crates/unpeel-cli/src/update.rs` and the toast UI were deleted
with the TUI, and there is currently no update-notification surface in the
CLI. The install-channel marker itself is unaffected:
`~/.unpeel/cli-install.json`, written by `install.sh`, still distinguishes an
installed build from a from-source checkout or the PTY test harness's
isolated `UNPEEL_HOME` — it is simply unread now that nothing checks for
updates.

## Server archives, `protocol/`, and the Mac app's server binaries

- **Every CLI archive ships `generated/`** — `generated/GeneratedRuntimeCatalog.swift`,
  the client-safe runtime catalog. The Apple clients in this tree consume the
  identical copy at `clients/shared/UnpeelShared/Sources/UnpeelShared/` (both are
  written by `bun run generate:runtimes` and verified by `bun run
  check:runtimes`); the archive copy exists for out-of-tree clients and
  humans. Same rules as `protocol/`: in the tar lists, in the required-entry
  check, ignored by `install.sh`.
- **Every CLI archive ships `protocol/`** — all of `protocol/*` (capability
  ledger, conformance fixtures, pane-layout operations, relay KAT vectors,
  `app-registry.json`, the UI stream fixtures) verbatim next to the three
  binaries, `LICENSE`, `THIRD_PARTY_NOTICES.txt`, and `BUILD_PROVENANCE.json`.
  `release-cli.mjs` and `build-cli-linux.sh` add the directory to the tar,
  and the publisher's required-entry check
  (`assertCliArchiveEntries` in `release-cli-state.mjs`) refuses an archive
  without `protocol/host-capabilities-v1.json` + `host-conformance-v1.json`.
  `install.sh` ignores the directory (it never installs it); it exists for
  out-of-tree clients that pin a server release and for humans. The Swift
  conformance tests in `apps/` read this checkout's `protocol/` directly.
- **Every versioned archive has an immutable `.sha256` sidecar** at
  `<channel>/cli/unpeel-<version>-<target>.tar.gz.sha256` (previously only
  `-latest` and revisioned keys had one). A Mac app build that bundles an
  archive verifies the exact versioned archive against it.
- **The Mac app bundles the server built from this tree.** `build-app.sh`
  cargo-builds `unpeel-host`, `unpeel`, and `unpeel-attach` (`--release
  --locked`, from `crates/` and `crates/unpeel-attach`) at the same commit as
  the app and the `unpeel-native-bridge` crate (a workspace member with path
  deps on `unpeel-core`/`unpeel-serve`), and collects their third-party
  notices with `unpeel-license-notices` exactly like `release:cli`. There is
  no server-version pin file and no bridge tag to re-pin: the crates workspace
  version is the only version, for the CLI archives and the app alike.
  `UNPEEL_SERVER_ARCHIVE=<local .tar.gz>` is the explicit opt-in that bundles
  a published `macos-universal` CLI archive instead (reproducibility checks,
  upgrade rehearsals): its `.sha256` sibling is verified when present and its
  `BUILD_PROVENANCE.json` must name the workspace version, `macos-universal`,
  and a 40-hex `source_commit` (echoed in the build log). Produce one with
  `bun run release:cli -- --channel beta --dry-run`. Because the app build
  no longer downloads a server archive, the CLI publish and the app cut are
  independent steps of one release: cut the CLI first (`release:cli`, all
  three targets), then the app (`release:mac`), then deploy the website.
- **The changelog lives with the website.** `release.sh` resolves it via
  `scripts/release-changelog.mjs`: `UNPEEL_CHANGELOG`, then
  `../unpeel-website/app/changelog.md` (the sibling checkout after the
  split), then `unpeel-website:apps/website/app/changelog.md` (monorepo), and fails naming
  the sibling checkout when none exists. Author the `## <version>` entry in
  the website before the app cut; deploy the website after.
- **The release Worker's deploy-time fallbacks are vendored**, not imported
  from the tree: `unpeel-website:apps/releases/scripts/vendor-fallbacks.mjs` copies
  `scripts/install.sh`, `scripts/install-app.sh`, and
  `protocol/app-registry.json` from `--from <server checkout>` (default: this
  repo root) into `unpeel-website:apps/releases/vendored/` (gitignored); `release:updates:*`
  and `unpeel-website:apps/releases` `npm run deploy[:dry-run]` run it first.
