# unpeel-usage

Local AI usage & credits at a glance — a small, fast terminal app
built entirely from native [Ratatui](https://ratatui.rs) layouts and widgets.
It reuses the logins and files your AI tools already keep on this machine:
no pasted API keys and no daemon. Claude live limits use Claude Code's stored
OAuth login; transcript history stays local.

```
  USAGE                                                  24h $12.15 est
────────────────────────────────────────────────────────────────────────
  Codex Pro                                              7-day 37% used
  Claude Max 20x          5-hour 15% · 7-day 44% · Fable 7-day 85% used
  Claude · work                     5h 28% · Fable 7-day 63% used

 j/k select · enter details · r refresh · a alerts · t theme · q quit
```

The selected provider gets the same full-width gray row and two-cell label
inset as `unpeel-tui-kit`'s Explorer. The list prioritizes quota readings over
account metadata: `5-hour` is Claude's rolling five-hour allowance, `7-day` is
the overall weekly allowance, and `Fable 7-day` is that model's weekly
allowance. Every percentage is the amount used. Email addresses and reset
dates/times stay in the detail view instead of crowding the list.

Press Enter to open the borderless detail view; a pinned, full-width `← Back`
row appears at the top and Enter, Escape, or a click returns to the list.
Detail quotas use a purpose-built Ratatui meter, calendar-day activity uses
the native `Sparkline` widget, and Claude pace projections keep their blue /
amber / red semantic states, spare estimate, run-out estimate, and even-pace
marker.

The shared design-system primitives come directly from
[`unpeel-tui-kit`](https://github.com/unpeel-com/unpeel-tui-kit):
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
  credit plans) the actual credits balance. No estimation.
- **Claude Code live limits** — the existing Claude Code OAuth credential is
  read from the macOS Keychain first, then `.credentials.json` as a fallback.
  The app requests Claude's usage endpoint for the real Session, Weekly,
  Sonnet/Fable scoped limits, Extra Usage, reset times, and plan name. Responses
  are cached for five minutes because the endpoint rate-limits aggressively.
- **Claude Code local history** — transcripts under `~/.claude/projects/`
  produce the Usage Trend and estimated Today, Yesterday, and Last 30 Days
  spend/token rows. Logs and calculated history never leave the machine.

Set `live_usage = false` under `[claude]` for a fully offline, transcript-only
Claude card. When live auth or the network is unavailable, the card keeps its
local history and falls back to a clearly labeled estimated Session spend row.

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

Alerts are an Unpeel App feature: the control is shown only when
`unpeel-usage` is running in an Unpeel-hosted session. Press `a` or click
**alerts** in the footer to open the Ratatui dialog. Its independent options
are all off by default:

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
registers itself as an Unpeel App (one manifest under
`~/.unpeel/apps/unpeel.app.usage/`): the session row takes the app's name
and amber tint — even when you just type `unpeel-usage` into any Unpeel
terminal — and the sidebar shows a live status line like
`Codex 3% · Claude $3.24`. The whole integration is `src/unpeel.rs` and
`src/install.rs`: plain files and one tiny local HTTP contract, freely
copyable into any app. There is no SDK.

When an Unpeel home exists (`$UNPEEL_HOME`, or `~/.unpeel`), its
`app-state.json` presets select and order the dashboard providers. Codex and
Claude are included at their first matching preset position; additional launch
variants are deduplicated, while all detected Claude accounts remain grouped
there. Without an Unpeel folder or a readable presets array, the standalone
Codex-then-Claude order is unchanged.

## Install

```sh
curl -fsSL https://unpeel.com/install/usage/install.sh | sh
```

Or build from source: `cargo build --release`.

## Commands

- `unpeel-usage` — the dashboard
- `unpeel-usage report` — one-shot plain-text snapshot for scripts and
  status bars (intentionally non-TUI so it remains pipe-friendly)

## Development

```sh
cargo run              # the dashboard, against your real local data
cargo run -- report    # one-shot text output (no TTY needed)
cargo test
```

Running any build once self-installs the App manifest into
`~/.unpeel/apps/unpeel.app.usage/` with that binary's absolute path as the
launch command — so after `cargo run`, typing
`target/debug/unpeel-usage` (or launching its seeded preset) inside Unpeel
shows the branded row, status line, and alerts against your dev build. The
manifest rewrites on every run, so release and debug builds simply take
over from each other.

Config lives at `~/.config/unpeel-usage/config.toml`; delete it to restore
defaults. Data is re-scanned every `refresh_secs` (and on `r`), and the
sidebar status line updates on every scan.
