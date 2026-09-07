//! Rich multi-line ListItem layouts in one scrollable Page, on the App runner.
//!
//! Run with `cargo run --example list_items` (also builds with
//! `--no-default-features`). Every row variant sits in one List: a captioned
//! divider row, plain, badge + trailing value, stacked detail, full-width
//! bottom Gauge, top Text band, leading and trailing media placeholders, a
//! plain divider row, busy, toggle, and a done/checkmark row.
//!
//! The `l layout` footer action cycles Inline, Stacked, and Auto (Auto stacks
//! below 70 columns, so resize the terminal to watch rows reflow).

use std::io;

use unpeel_app_kit::{
    App, AppAction, AppMetadata, Badge, Checkmark, FooterAction, Gauge, List, ListItem,
    ListItemBand, ListItemEmphasis, ListItemMedia, ListItemSlot, ListItemTone, ListRowLayout, Page,
    Reduce, Sparkline, Spinner, Toggle, run_app,
};

const AUTO_THRESHOLD: u16 = 70;

struct Showcase {
    layout: ListRowLayout,
    quota: f64,
    done: bool,
    checked: bool,
    spinner: Spinner,
}

impl Showcase {
    fn layout_name(&self) -> &'static str {
        match self.layout {
            ListRowLayout::Inline => "inline",
            ListRowLayout::Stacked => "stacked",
            ListRowLayout::Auto { .. } => "auto",
        }
    }
}

impl App for Showcase {
    fn page(&self) -> Page {
        let percent = (self.quota * 100.0).round();
        let items = vec![
            ListItem::divider_labeled("sep-rows", "Rows"),
            ListItem::new("plain", "Plain single-line row").activate_action("open"),
            ListItem::new("badge", "Codex")
                .emphasis(ListItemEmphasis::Strong)
                .accessory(ListItemSlot::badge(
                    Badge::new("Pro").tone(ListItemTone::Accent),
                ))
                .value("42% left · resets in 2h 10m · 5h window")
                .value_min_width(0)
                .activate_action("open"),
            ListItem::new("stacked", "Claude")
                .emphasis(ListItemEmphasis::Strong)
                .detail("Weekly limit resets Thursday, session limit resets in 3h")
                .value("18% left")
                .value_tone(ListItemTone::Warning)
                .activate_action("open"),
            ListItem::new("gauge", "Weekly quota")
                .detail("7-day rolling window · Enter bumps it")
                .bottom(ListItemBand::gauge(
                    Gauge::new(
                        "weekly-gauge",
                        self.quota,
                        "7-day limit",
                        format!("{percent} percent left"),
                    )
                    .caption(format!("{percent}% left")),
                ))
                .activate_action("bump-quota"),
            ListItem::new("band", "Release 1.4.0")
                .top(ListItemBand::text(
                    "Shipped yesterday · 3 fixes",
                    ListItemTone::Success,
                ))
                .value("v1.4.0")
                .activate_action("open"),
            ListItem::new("leading-media", "Ada Lovelace")
                .emphasis(ListItemEmphasis::Strong)
                .detail("Analytical Engine notes · 2 unread")
                .media(
                    ListItemMedia::leading(4)
                        .glyph("AL")
                        .tone(ListItemTone::Info),
                )
                .activate_action("open"),
            ListItem::new("trailing-media", "Screenshot.png")
                .detail("1.2 MB · edited 4 min ago")
                .media(
                    ListItemMedia::trailing(6)
                        .glyph("▣")
                        .tone(ListItemTone::Accent),
                )
                .bottom(ListItemBand::sparkline(Sparkline::new(
                    "views",
                    vec![1.0, 3.0, 2.0, 5.0, 4.0, 6.0, 5.0, 7.0, 6.0, 8.0],
                    "views over the last ten days",
                )))
                .activate_action("open"),
            ListItem::divider("sep-state"),
            ListItem::new("busy", "Syncing workspace")
                .busy(true)
                .detail("waiting for the remote"),
            ListItem::new("toggle", "Write the release notes")
                .done(self.done)
                .leading(ListItemSlot::toggle(Toggle::new(
                    "toggle-notes",
                    "Completed",
                    self.done,
                    "set-done",
                )))
                .detail("Space or Enter flips it"),
            ListItem::new("checkmark", "Dark theme")
                .detail("selection-mode row")
                .checkmark(Checkmark::new(
                    "check-theme",
                    "Selected",
                    self.checked,
                    "set-theme",
                )),
        ];
        Page::new(
            "Rich rows",
            List::new("rows", items)
                .row_layout(self.layout)
                .scroll_padding(1),
        )
        .footer_actions([
            FooterAction::new("layout", self.layout_name(), "cycle-layout").accelerator("l"),
        ])
    }

    fn reduce(&mut self, action: AppAction) -> Reduce {
        match action {
            AppAction::Command { action } if action == "cycle-layout" => {
                self.layout = match self.layout {
                    ListRowLayout::Inline => ListRowLayout::Stacked,
                    ListRowLayout::Stacked => ListRowLayout::Auto {
                        stack_below_width: AUTO_THRESHOLD,
                    },
                    ListRowLayout::Auto { .. } => ListRowLayout::Inline,
                };
            }
            AppAction::Toggle { control, on, .. } if control == "toggle-notes" => self.done = on,
            AppAction::Toggle { control, on, .. } if control == "check-theme" => {
                self.checked = on;
            }
            AppAction::Activate { action, .. } if action == "bump-quota" => {
                self.quota = (self.quota + 0.1) % 1.0;
            }
            AppAction::Cancel => return Reduce::Quit,
            _ => return Reduce::Ignored,
        }
        Reduce::Changed
    }

    fn tick(&mut self) -> bool {
        self.spinner.tick();
        false
    }

    fn spinner_frame(&self) -> usize {
        self.spinner.frame()
    }
}

fn main() -> io::Result<()> {
    let app = Showcase {
        layout: ListRowLayout::Auto {
            stack_below_width: AUTO_THRESHOLD,
        },
        quota: 0.61,
        done: true,
        checked: false,
        spinner: Spinner::new(),
    };
    run_app(
        app,
        AppMetadata::new(
            "dev.unpeel.app-kit.list-items",
            "Rich rows",
            env!("CARGO_PKG_VERSION"),
        ),
    )
}
