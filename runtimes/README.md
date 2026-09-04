# Built-in agent runtimes

Each directory in this folder is one built-in agent runtime package. The Rust
build discovers `runtime.toml` files automatically, validates them, and emits
the compiled registry. Adding a package must not require adding its name to a
central provider list.

This is a source contribution boundary: a new or changed runtime ships in a
new Unpeel build. Downloadable third-party adapters are not supported yet.

## Package layout

```text
runtimes/<slug>/
├── runtime.toml
├── adapter/
│   ├── mod.rs
│   ├── setup.rs          # optional: hooks, wrappers, config merge, MCP
│   ├── resume.rs         # optional: resume/fresh/launch identity
│   └── transcript.rs     # optional: transcript discovery and parsing
├── assets/
│   ├── icon.svg          # optional client-embedded runtime mark
│   └── hooks/            # optional scripts, plugins, and wrappers
└── fixtures/             # add provider-owned fixtures as behavior grows
```

Provider-neutral enforcement remains in `unpeel-core`: PTY ownership,
PID/start-time checks, hook ingress and generation ordering, locked/atomic
file writes, MCP authorization, transcript path validation and read bounds,
activity arbitration, notifications, and the Host/Controller protocol.
Runtime adapters return plans or normalized data; they do not weaken those
boundaries.

## Descriptor

`runtime.toml` is the source of truth for metadata and declared capabilities.
The schema is strict: unknown fields, duplicate identities or aliases, an
invalid directory/slug pair, and inconsistent lifecycle/capability claims
fail the build.

Important fields:

- `id`: stable reverse-DNS identity. Never derive this from a display label.
- `slug`: source package directory name.
- `legacy_slug`: compatibility identity used by existing manifests and wire
  fields. Keep it stable during the catalog migration.
- `adapter`: optional reviewed compiled adapter (`builtin:<legacy_slug>`).
- `legacy_order`: compatibility ordering for built-ins and seeded presets.
- `platforms`: Host targets where the runtime can be launched. Descriptors and
  icons remain available to every Controller for cross-Host presentation, but
  adapter code, local setup scans, and seeded presets are compiled/exposed
  only on the declared targets.
- `display`, `install`, and `suggested_presets`: generated into client-safe
  metadata so Mac, iOS, and headless serve do not grow new provider tables. `display.kind`
  is the presentation family (`agent`, `app`, `editor`, `terminal`; default
  `agent`) so a markdown-editor or Unpeel App CLI gets the right generic logo
  without a client special case. A custom `display.icon_asset` must be a
  package-local SVG below `assets/` and declare `icon_source` plus
  `icon_license`. Icons default to template rendering; set
  `icon_template = false` only when authored fills must be preserved.
  `display.window_padding_x` is the Ghostty side inset in points (0–48,
  default 0 = edge-to-edge). Agent runtimes currently use 8 by convention;
  full-bleed TUIs such as Grok and OpenCode omit it.
- `detection`: conservative command/process aliases, package path signatures,
  and optional home-relative executable search paths.
- `environment.strip_inherited`: provider identity/session variables that a
  nested Unpeel Host must remove before opening a new terminal.
- `usage.stores`: optional bounded, home-relative session-file patterns used
  only to rank an existing user's agents during first-run preset seeding.
- `lifecycle`: source, authority, fallback, reliability claims, and the
  controller-side output semantics used to restore hook state
  (`anchor_start_event_to_output`, `attention_clears_on_output`, and
  `distrust_stops_while_output_grows`). The output flags default to
  `true`, `true`, and `false`; declare only runtime-specific exceptions.
  `authority = "none"` must use `fallback = "none"`: raw output/screen
  changes remain telemetry and never start animated Busy.
- `capabilities`: only behavior actually implemented by the adapter.

Current capabilities are `lifecycle_hooks`, `resume`, `restart_agent`,
`mcp_sessions`, `mcp_browser`, `mcp_computer`, `transcript`, and
`notify_when_done`.

`restart_agent` is the v1 source-schema spelling retained for built-in package
compatibility. It enables the user-facing **Resume Agent** recipe only after a
managed runtime returns to its owned shell; it never authorizes interrupting an
active runtime.

## Adding a runtime

1. Research the provider before writing code: executable/wrapper signatures,
   official install path, lifecycle events, conversation identity, exact
   resume semantics, MCP configuration, transcript roots and format, and any
   version-dependent behavior.
2. Copy the smallest similar runtime directory, then replace its descriptor.
   Keep aliases exact and add false-positive tests for generic executable
   names or wrappers.
3. Add only the adapter modules the provider needs. A runtime with no safe
   hook or resume primitive should omit that capability instead of emulating
   a stronger integration.
4. Put provider-owned scripts, plugins, and optional `icon.svg` in `assets/`.
   Load setup assets with `include_str!`; the catalog generator embeds the
   declared icon for shared clients. Record an upstream URL or explicit
   `internal:` generation/migration marker and its license/brand status.
   Installers must merge user configuration idempotently, preserve unrelated
   entries, and remove only Unpeel-owned entries.
5. Every owned lifecycle reporter must send and durably seed numeric
   `unpeel_runtime_generation`. It must no-op outside an Unpeel Session,
   report to the direct hook port and current port registry, and forward only
   the provider conversation ID/path fields the Host knows how to validate.
6. Automatic MCP setup must use the provider's additive mechanism and must
   report registration evidence per domain. A Session grant is not proof that
   an MCP client was configured.
7. Resume/restart code must preserve the original semantic command, support
   only verified identity modes (exact ID, documented continue-last, picker,
   or pinned storage), and never turn passive process observation into a
   launch recipe.
8. Transcript code returns normalized records and provider path claims. Core
   still canonicalizes roots, rejects traversal/symlink escape, and applies
   read/search limits.
9. Regenerate client metadata with `bun run generate:runtimes`, then run
   `bun run validate:runtimes` to validate the schema/generated registry and
   prove there is no client-catalog drift.

## Integration levels

A runtime does not need Claude-level capabilities to be valid:

- A generic preset runs any command in a durable terminal.
- Detection adds an active logo/tint while a matching foreground process is
  present. Detection alone is presentation-only.
- A managed built-in may add setup, lifecycle, MCP, conversation resume,
  context, fork, and transcript support independently.

Capability honesty is more important than feature parity. In particular, an
agent typed later into a blank Terminal is still an observed occupant: it does
not inherit the package's hooks, MCP injection, conversation binding, or
Resume Agent action unless Unpeel owned its launch.

## Verification

At minimum, add package-local unit fixtures and run:

```sh
bun run validate:runtimes
cargo test -p unpeel-core
cargo test -p unpeel-host
cargo test -p unpeel-cli
# The Apple repo (unpeel-apple) runs its own client suites against the catalog
# regenerated with --out from this checkout
```

Deep integrations also need a real hosted-PTY proof covering launch, lifecycle
busy/stop, captured conversation identity, same-PTY Resume Agent after the
managed runtime returns to its shell, stale hook generation rejection, and
transcript resolution. Keep the blank-terminal
negative proof: detection may change presentation but must not promote the
saved blank launch.
