# unpeel-usage

Local AI usage & credits at a glance — a small, fast terminal dashboard
built with [Ratatui](https://ratatui.rs). Everything is read from files your
AI tools already write on this machine: no API keys, no network, no daemon.

```
 Usage
────────────────────────────────────────────
▎Codex                                  pro
   week     █░░░░░░░░░░ 3% · resets 6d 18h
   credits                            $436

 Claude Code
   5h block     $3.24 est · resets 1h 20m
   24h               $8.91 est · 1.2M tok

 j/k select · enter details · q quit
```

## What it reads

- **Codex CLI** — the rollout logs under `~/.codex/sessions/` record a rate-limit
  snapshot with every turn: real window utilization, reset times, and (on
  credit plans) the actual credits balance. No estimation.
- **Claude Code** — the transcripts under `~/.claude/projects/` carry per-message
  token usage. Claude records no quota locally, so unpeel-usage shows
  **estimated** spend from public per-model API prices: the rolling 24h total
  and the current 5-hour billing block, with its reset time.

## Alerts

`~/.config/unpeel-usage/config.toml` (written with commented defaults on
first run):

- `codex_used_percent` — alert when a Codex window crosses this utilization
- `credits_low_usd` — alert when the Codex credits balance drops to this
- `claude_block_usd` — a personal budget line for the Claude 5h block

Press `a` to toggle alerts for the session. Standalone, an alert shows in
the dashboard; inside [Unpeel](https://unpeel.com) it raises the session's
attention state, which means the sidebar accent plus desktop and phone
notifications.

## Unpeel

`unpeel-usage` is a standalone tool first. When Unpeel is installed it also
registers itself as an Unpeel App (one manifest under
`~/.unpeel/apps/unpeel.app.usage/`): the session row takes the app's name
and amber tint — even when you just type `unpeel-usage` into any Unpeel
terminal — and the sidebar shows a live status line like
`Codex 3% · Claude $3.24`. The whole integration is `src/unpeel.rs` and
`src/install.rs`: plain files and one tiny local HTTP contract, freely
copyable into any app. There is no SDK.

## Install

```sh
curl -fsSL https://unpeel.com/install/usage/install.sh | sh
```

Or build from source: `cargo build --release`.

## Commands

- `unpeel-usage` — the dashboard
- `unpeel-usage report` — one-shot plain-text snapshot for scripts and
  status bars

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
