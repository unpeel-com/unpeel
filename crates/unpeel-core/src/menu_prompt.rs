//! Detection for agent-drawn "pick an option" select menus (Claude/Codex
//! numbered prompts). These are painted inside the provider CLI's own TUI and
//! fire **no** lifecycle hook — no `Stop`, no `PermissionRequest` — so the
//! activity engine has no signal that the session is waiting for a choice and
//! keeps showing "busy". The only reliable tell is the rendered screen text,
//! so this scans the visible viewport for a menu's footer hints.
//!
//! This is the host-side twin of the iOS `RemoteGhosttyRenderer` scan
//! (`menuPromptActive` in `RemoteGhosttyTerminalView.swift`): the marker lists
//! are intentionally kept in sync so the phone's on-screen menu control bar and
//! the desktop's attention badge fire on exactly the same prompts. Keep the two
//! lists aligned when either side changes.
//!
//! Provider-agnostic and hook-free by construction: it only trusts the
//! on-screen text, so it works for any CLI that draws a keyboard-navigable
//! select menu. Detection requires BOTH a navigation hint and a select/cancel
//! hint on screen at once, so ordinary prose that merely mentions "select"
//! cannot trip it.

/// Footer phrases advertising arrow-key navigation.
const NAV_MARKERS: &[&str] = &["to navigate", "↑/↓", "↑ ↓", "▲/▼", "arrow keys"];

/// Footer phrases advertising a confirm/cancel key.
const SELECT_MARKERS: &[&str] = &[
    "to select",
    "to confirm",
    "to choose",
    "enter to",
    "esc to cancel",
    "return to",
];

/// Confirm-key phrases for footers that name a confirm and a cancel key but
/// no navigation hint at all — Codex's approval menu prints only
/// "Press enter to confirm or esc to cancel" under its numbered options.
const CONFIRM_MARKERS: &[&str] = &["enter to confirm", "return to confirm"];

/// Cancel-key phrases paired with `CONFIRM_MARKERS`.
const CANCEL_MARKERS: &[&str] = &["esc to cancel", "escape to cancel"];

/// Phrases that mark a hint row as a passive status footer rather than an
/// answerable menu. Claude Code's subagent list pins
/// "↑/↓ to select · Enter to view" to the bottom for the whole run — it has
/// both a nav and a select marker but nothing is waiting for a choice.
const PASSIVE_MARKERS: &[&str] = &["to view"];

/// Prefixes whose incomplete repaint is already identifiable as Claude's
/// passive subagent selector. Claude paints this row progressively, so a scan
/// can land after "↑/↓ to select" but before "Enter to view". Treat that
/// ambiguous prefix as passive unless the same footer also advertises an
/// action that only an answerable menu has.
const PASSIVE_SELECTOR_PREFIXES: &[&str] = &["↑/↓ to select"];

/// Phrases that disambiguate a `PASSIVE_SELECTOR_PREFIXES` footer as an
/// answerable menu. Keep these narrower than `SELECT_MARKERS`: the bare
/// "Enter to" prefix is exactly the partial-paint state we must not alert on.
const INTERACTIVE_QUALIFIERS: &[&str] = &[
    "to navigate",
    "to confirm",
    "to choose",
    "esc to cancel",
    "escape to cancel",
];

/// True when the visible screen text looks like an interactive select menu
/// waiting for a keyboard choice. `screen_text` is the rendered viewport
/// (visible rows only), newline-separated.
///
/// The hints must appear on the same or adjacent rows — menu footers are one
/// hint line (two when wrapped), while a nav phrase in transcript prose plus
/// an unrelated select phrase elsewhere on screen is not a menu. Two footer
/// shapes qualify: a nav hint plus a select hint (Claude-style), or a confirm
/// key named next to a cancel key (Codex-style, which prints no nav hint).
pub fn viewport_has_menu_prompt(screen_text: &str) -> bool {
    let lines: Vec<String> = screen_text
        .lines()
        .map(|line| line.to_lowercase())
        .collect();
    for (index, line) in lines.iter().enumerate() {
        let window = match lines.get(index + 1) {
            Some(next) => format!("{line}\n{next}"),
            None => line.clone(),
        };
        // A narrow terminal (phone fit) can wrap a footer mid-phrase, e.g.
        // "… · Enter to\n   view" — collapse whitespace runs so multi-word
        // markers still match across the wrap. Without this the passive
        // "to view" guard misses and the pinned subagent footer reads as an
        // answerable menu.
        let window = window.split_whitespace().collect::<Vec<_>>().join(" ");
        let has_nav = NAV_MARKERS.iter().any(|marker| window.contains(marker));
        let has_select = SELECT_MARKERS.iter().any(|marker| window.contains(marker));
        let has_confirm = CONFIRM_MARKERS.iter().any(|marker| window.contains(marker));
        let has_cancel = CANCEL_MARKERS.iter().any(|marker| window.contains(marker));
        let passive_action = PASSIVE_MARKERS.iter().any(|marker| window.contains(marker));
        let passive_selector_prefix = PASSIVE_SELECTOR_PREFIXES
            .iter()
            .any(|marker| window.contains(marker));
        let interactive_qualifier = INTERACTIVE_QUALIFIERS
            .iter()
            .any(|marker| window.contains(marker));
        let passive = passive_action || (passive_selector_prefix && !interactive_qualifier);
        if ((has_nav && has_select) || (has_confirm && has_cancel)) && !passive {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_claude_style_menu_footer() {
        let screen = "❯ 1. Switch to $59/year\n  2. Keep perpetual one-time\n\n\
             Enter to select · ↑/↓ to navigate · Esc to cancel";
        assert!(viewport_has_menu_prompt(screen));
    }

    #[test]
    fn detects_codex_confirm_cancel_footer() {
        // Codex's approval menu prints no navigation hint at all — only a
        // confirm/cancel footer under the numbered options.
        let screen = "  1. Yes, proceed\n\
             2. No, and tell Codex what to do differently (esc)\n\n\
             Press enter to confirm or esc to cancel";
        assert!(viewport_has_menu_prompt(screen));
    }

    #[test]
    fn a_lone_cancel_hint_is_not_a_menu() {
        // A cancel key without a confirm key (e.g. an interrupt hint while
        // the agent works) must not trip detection.
        assert!(!viewport_has_menu_prompt("Working… press esc to cancel"));
    }

    #[test]
    fn detects_arrow_keys_phrasing() {
        let screen = "Use arrow keys to move.\nPress return to confirm your choice.";
        assert!(viewport_has_menu_prompt(screen));
    }

    #[test]
    fn requires_both_a_nav_and_a_select_hint() {
        // Prose that only mentions selecting must not trip detection.
        assert!(!viewport_has_menu_prompt(
            "Please select the files you want to keep and let me know."
        ));
        // A navigation hint alone (e.g. a scroll hint) is not a menu.
        assert!(!viewport_has_menu_prompt(
            "Use ↑/↓ to navigate the log output."
        ));
    }

    #[test]
    fn ignores_claude_subagent_status_footer() {
        // Claude Code pins this while subagents run — nav + select markers,
        // but a passive "view" footer, not a menu waiting for an answer.
        let screen = "⏺ Working…\n\n\
             ⏺ main   ↑/↓ to select · Enter to view  ◯ Explore  Audit resume pipeline";
        assert!(!viewport_has_menu_prompt(screen));
    }

    #[test]
    fn ignores_claude_subagent_footer_wrapped_at_phone_width() {
        // The same pinned subagent footer at a 44-column phone fit: the wrap
        // splits "Enter to view" between the rows, which used to defeat the
        // passive guard while "↑/↓" and "to select" still matched.
        let screen = "  ◯ main           ↑/↓ to select · Enter to\n\
                   view\n\
             \u{23fa} general-purpose 55m 46s · ↓ 348.3k";
        assert!(!viewport_has_menu_prompt(screen));
    }

    #[test]
    fn ignores_claude_subagent_footer_during_partial_repaint() {
        // The Host and iOS renderer can scan between synchronized repaint
        // chunks. This prefix must not create a brief false attention edge
        // before the wrapped "view" continuation arrives.
        let screen = "  ⏺ main           ↑/↓ to select · Enter to";
        assert!(!viewport_has_menu_prompt(screen));
    }

    #[test]
    fn detects_qualified_menu_with_same_arrow_select_prefix() {
        // A real menu may use the same opening phrase; an explicit
        // confirm/cancel action disambiguates it from Claude's subagent list.
        let screen = "  1. Keep working\n  2. Stop\n\
             ↑/↓ to select · Enter to confirm · Esc to cancel";
        assert!(viewport_has_menu_prompt(screen));
    }

    #[test]
    fn requires_hints_on_same_or_adjacent_rows() {
        // A nav phrase in transcript prose plus an unrelated select phrase
        // rows apart is not a menu footer.
        let screen = "The arrows are now first: ← ↑ ↓ →\n\
             (compiling)\n(compiling)\n(compiling)\n\
             Press Enter to submit your prompt.";
        assert!(!viewport_has_menu_prompt(screen));
    }

    #[test]
    fn empty_screen_is_not_a_menu() {
        assert!(!viewport_has_menu_prompt(""));
    }
}
