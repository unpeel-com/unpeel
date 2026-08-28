# unpeel-usage

Local AI usage & credits at a glance — a small, fast terminal app
built entirely from native [Ratatui](https://ratatui.rs) layouts and widgets.
It reuses the logins and files your AI tools already keep on this machine:
no pasted API keys and no daemon. Claude and Grok live limits reuse their CLI
logins; Codex, Claude, Grok, and Muse history stays local.

```
  Codex Pro                                              7-day 37% used
  Claude Max 20x          5-hour 15% · 7-day 44% · Fable 7-day 85% used
  Claude · work                     5h 28% · Fable 7-day 63% used
  Grok SuperGrok Heavy                                  7-day 22% used
  Muse Spark 1.2                                $1.24 est · 4.8M tokens
  Current project                                      2.1M tokens
  Total usage                                          12.7M tokens
```

## Install

```sh
curl -fsSL https://unpeel.com/install/usage/install.sh | sh
```

The checksum-verified installer places `unpeel-usage` on `PATH`, where Unpeel
detects it automatically. No registration command or `~/.unpeel/apps` write
is needed. To build and install from source, keep App Kit beside the App repo:

```sh
mkdir -p ~/Dev && cd ~/Dev
git clone https://github.com/unpeel-com/unpeel-app-kit.git
git clone https://github.com/unpeel-com/unpeel-app-usage.git
cargo install --locked --path unpeel-app-usage
```

The selected provider gets the same full-width gray row and two-cell label
inset as `unpeel-app-kit`'s Explorer. The list prioritizes quota readings over
account metadata: `5-hour` is Claude's rolling five-hour allowance, `7-day` is
the overall weekly allowance, and `Fable 7-day` is that model's weekly
allowance. Every percentage is the amount used. Email addresses and reset
dates/times stay in the detail view instead of crowding the list.
Unpeel's Session title owns the App name, so there is no repeated in-App title.
The only footer copy is the compact `a alert  r refresh` action row (with a
spinner replacing `refresh` while a scan is active).

Press Enter to open the borderless detail view; a pinned, transparent `← Back`
action appears at the top and Enter, Escape, or its full-width click target
returns to the list.
Detail quotas use a purpose-built Ratatui meter, calendar-day activity uses
the native `Sparkline` widget, and Claude pace projections keep their blue /
amber / red semantic states, spare estimate, run-out estimate, and even-pace
marker.

The **Current project** row attributes local history to the Git project from
which each agent session was launched. In Unpeel, every background refresh
resolves the App's Host-owned `AppContext::current_root()`, so the row follows
the Session's project/worktree even when the App process directory differs.
A standalone run uses its process working directory. Worktrees are folded
into their main repository, and the row opens to the same exact monthly table.

The final **Total usage** row adds the local token histories from all four
providers. Open it to see this month's Usage by project first, followed by a
newest-first Month / Tokens table for the current month and the previous 11
months. These are processed tokens, including cached context—not an API bill
or a subscription charge. History or working-directory metadata absent from
local logs cannot be reconstructed.

The shared design-system primitives come directly from
[`unpeel-app-kit`](https://github.com/unpeel-com/unpeel-app-kit):
`SELECTABLE_LEFT_PADDING`, the dark and light `KitTheme` selection colors,
and `VerticalScrollbar`. Usage retains its OSC 11 appearance detection; when
the terminal cannot report an appearance, the selected row uses
terminal-native reverse video instead of assuming a dark background.

## Light and dark themes

The dashboard queries the terminal's background color when it starts, then
selects a complete light or dark palette. If that query is unsupported it uses
`COLORFGBG` where reliable; otherwise it falls back to terminal-default colors,
which follow the host theme without needing detection. Inside Unpeel, the live
background query takes priority because `COLORFGBG` records the appearance from
when that shell started and cannot change with the surrounding pane.

Set the top-level config value to force a palette:

```toml
theme = "light" # "auto", "light", or "dark"
```

For a one-off override, use `UNPEEL_USAGE_THEME=light unpeel-usage`. The
environment variable accepts the same three values and takes precedence over
`~/.config/unpeel-usage/config.toml`. Press `t` in the dashboard to cycle
adaptive, light, and dark palettes for the current session.

The provider detail hierarchy is inspired by the grouped dashboard in
[OpenUsage](https://github.com/robinebers/openusage), adapted for terminal
cells and narrow viewports.

The app is fully mouse-aware: click a row to select it, click it again for
details, click the Back row to return, use the wheel to scroll by rows, or
click and drag the scrollbar. The keyboard does everything too (`j/k`,
`enter`, `esc`, `PageUp/PageDown`, `r`, `a`, `t`, `q`). The selected provider
returns to view after keyboard navigation.

## What it reads

- **Codex CLI** — the rollout logs under `~/.codex/sessions/` record a rate-limit
  snapshot with every turn: real window utilization, reset times, and (on
  credit plans) the actual credits balance. No estimation. Session metadata
  supplies its working directory for project attribution.
- **Claude Code live limits** — the existing Claude Code OAuth credential is
  read from the macOS Keychain first, then `.credentials.json` as a fallback.
  The app requests Claude's usage endpoint for the real Session, Weekly,
  Sonnet/Fable scoped limits, Extra Usage, reset times, and plan name. Responses
  are cached for five minutes because the endpoint rate-limits aggressively.
- **Claude Code local history** — transcripts under `~/.claude/projects/`
  produce the Usage Trend and estimated Today, Yesterday, and Last 30 Days
  spend/token rows. Their recorded working directory supplies project
  attribution. Logs and calculated history never leave the machine.
- **Grok live limits** — `~/.grok/auth.json` supplies the Grok CLI access and
  refresh tokens. The app makes the same credits-format billing request as the
  CLI for the weekly shared pool and Extra Usage cap status, plus the settings
  request for the plan name. Rotated credentials are atomically written back
  without dropping other accounts or unknown fields.
- **Grok local history** — completed turns under
  `~/.grok/sessions/**/updates.jsonl` (or `$GROK_HOME/sessions`) produce the
  Usage Trend and Today / Yesterday / Last 30 Days rows. Grok's recorded turn
  cost is preferred and reasoning tokens are not counted twice. Coordinator
  totals exclude their subagent ledgers, copied event IDs are deduplicated, and
  unchanged logs are served from an in-process incremental cache. Each
  session's summary supplies its working directory for project attribution.
- **Muse local history** — provider-attributed model calls in
  `~/.local/share/muse/sessions/**/session.jsonl` supply token totals and
  estimated spend using Muse's locally recorded model catalog prices. Usage
  IDs are deduplicated across coordinator and subagent event logs so mirrored
  attribution records are counted once. Muse authentication never leaves its
  own CLI. Workspace metadata supplies project attribution.

Set `live_usage = false` under `[claude]` for a fully offline, transcript-only
Claude card. When live auth or the network is unavailable, the card keeps its
local history and falls back to a clearly labeled estimated Session spend row.

Grok has the equivalent opt-out:

```toml
[grok]
live_usage = false
```

This disables only the network-backed Weekly / Extra Usage / plan lookup. Grok
session history remains local and available.

## Multiple Claude accounts

The normal account-switching workflow is supported: use `/logout`, then sign in
to the next Claude account in the same `~/.claude` profile. On refresh,
unpeel-usage recognizes the changed account email and keeps the previous
account as a **saved** card. The signed-in account is live; logged-out cards
show their last successful limits and update again the next time that account
is signed in.

Only the email, plan, limits, and fetch time are retained for up to 90 days in
`~/Library/Caches/unpeel-usage/claude-accounts.json`. OAuth access and refresh
tokens are never copied. Because shared Claude transcripts contain no account
identity, Usage Trend and the Today / Yesterday / Last 30 Days estimates remain
combined and appear only on the active profile card.

Separate Claude Code config directories also get independent cards. A live
card shows the subscription plan in its header; account email and source detail
remain available with Enter:

- `~/.claude` — the default account (or `$CLAUDE_CONFIG_DIR` when set)
- `~/.claude-*` — the common convention for second accounts run with
  `CLAUDE_CONFIG_DIR=~/.claude-work claude`; detected automatically
- anything listed under `[claude] dirs` in the config file, for dirs that
  live elsewhere:

```toml
[claude]
live_usage = true
dirs = ["~/claude-accounts/personal"]
```

## Alerts

Alerts are an Unpeel App feature available only when `unpeel-usage` is running
in an Unpeel-hosted session. Press `a` to open the Ratatui dialog. Its
independent options are all off by default:

- **Close to a limit** — 80% used or pacing that projects an early run-out
- **Limit reached** — a bounded quota reaches 100%
- **Available again** — a previously constrained quota resets

The dialog changes the current session. The matching booleans under `[alerts]`
in `~/.config/unpeel-usage/config.toml` can opt in by default on future runs.
Enabled events create a first-class Unpeel **Alert**: it appears in Recent and
the desktop/mobile activity dropdowns, and the native Unpeel Host delivers its
own macOS banner and phone push. Alerts do not change the session's Busy, Idle,
or Attention state. Standalone runs have no alert control and send no
notifications.

The same config section retains `codex_used_percent`, `credits_low_usd`, and
`claude_block_usd` for card severity and personal budget thresholds.

## Unpeel

`unpeel-usage` is a standalone tool first. When Unpeel is installed it also
detects the `unpeel-usage` CLI directly from `PATH`: the session row takes the
App's name and live project/workspace accent, and the sidebar shows a status
line like `Codex 3% · Claude $3.24 · Grok 14% · Muse $1.20`. App Kit's shared
`AppReporter` owns the small plain-file plus loopback-HTTP integration; the
App itself remains standalone-safe.

When an Unpeel home exists (`$UNPEEL_HOME`, or `~/.unpeel`), its
`app-state.json` presets select and order the dashboard providers. Codex,
Claude, Grok, and Muse are included at their first matching preset position;
additional launch variants are deduplicated, while all detected Claude
accounts remain grouped there. Without an Unpeel folder or a readable presets
array, the standalone order is Codex, Claude, Grok, then Muse. The synthesized
Current project and Total usage rows stay last in either mode.

## Commands

- `unpeel-usage` — the dashboard
- `unpeel-usage report` — one-shot plain-text snapshot for scripts and
  status bars (intentionally non-TUI so it remains pipe-friendly)
- `unpeel-usage --version` — print the installed App version

## Development

```sh
cargo run              # the dashboard, against your real local data
cargo run -- report    # one-shot text output (no TTY needed)
cargo test
```

To use a development build inside Unpeel, put its output directory on `PATH`;
the Host then discovers it through the same central CLI catalog as an
installed build, without a registration write:

```sh
cargo build
PATH="$PWD/target/debug:$PATH" unpeel-usage
```

Config lives at `~/.config/unpeel-usage/config.toml`; delete it to restore
defaults. Data is re-scanned every `refresh_secs` (and on `r`), and the
sidebar status line updates on every scan. The bottom row keeps `a alert` and
`r refresh` visible inside Unpeel, using the stronger foreground only for the
shortcut letters. While either a manual or scheduled scan is actually
running, `r refresh` becomes `r` plus an animated spinner and `refreshing…`
status.
