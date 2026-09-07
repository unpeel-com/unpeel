# Writing an App

An App Kit App is one struct with two functions: `page()` builds the screen
from your model and `reduce()` applies one action to the model. The runner
owns the terminal, keys, mouse, hosted renderers, and change publishing.

## A complete App

```rust
use unpeel_app_kit::{
    App, AppAction, AppMetadata, Input, List, ListItem, ListItemSlot, Page, Reduce, Toggle,
    run_app,
};

struct Todos {
    items: Vec<(u64, String, bool)>, // id, label, done
    selected: Option<u64>,
    next_id: u64,
}

impl App for Todos {
    fn page(&self) -> Page {
        let rows = self.items.iter().map(|(id, label, done)| {
            ListItem::new(format!("todo-{id}"), label.clone())
                .done(*done)
                .trailing(ListItemSlot::toggle(Toggle::new(
                    format!("todo-{id}-toggle"), "Completed", *done, "set-done",
                )))
                .delete_action("delete-todo")
        });
        let mut list = List::new("todos", rows.collect()).empty_message("No todos yet");
        if let Some(id) = self.selected {
            list = list.selected(format!("todo-{id}"), "select-todo");
        }
        Page::new("Todos", list)
            .input(Input::new("new-todo", "New todo").submit_action("add-todo"))
    }

    fn reduce(&mut self, action: AppAction) -> Reduce {
        let id = |item: &str| item.trim_start_matches("todo-").parse::<u64>().ok();
        match action {
            AppAction::Submit { text, .. } if !text.trim().is_empty() => {
                self.items.push((self.next_id, text.trim().to_owned(), false));
                self.next_id += 1;
            }
            AppAction::Toggle { item, on, .. } => {
                if let Some(row) = id(&item).and_then(|id| self.items.iter_mut().find(|r| r.0 == id)) {
                    row.2 = on;
                }
            }
            AppAction::Delete { item } => {
                let id = id(&item);
                self.items.retain(|row| Some(row.0) != id);
            }
            AppAction::Select { item } => self.selected = id(&item),
            AppAction::Cancel => return Reduce::Quit,
            _ => return Reduce::Ignored,
        }
        Reduce::Changed
    }
}

fn main() -> std::io::Result<()> {
    let app = Todos { items: Vec::new(), selected: None, next_id: 1 };
    run_app(app, AppMetadata::new("dev.example.todos", "Todos", "0.1.0"))
}
```

That is the whole program. `cargo run` gives a keyboard and mouse driven
terminal UI; inside an Unpeel Host the same binary is also rendered natively.

## The App trait

```rust
pub trait App {
    fn page(&self) -> Page;                          // the screen for the current model
    fn reduce(&mut self, action: AppAction) -> Reduce; // apply one action
    fn tick(&mut self) -> bool { false }             // ~80 ms idle hook; true = rebuild page
    fn spinner_frame(&self) -> usize { 0 }           // frame for busy rows and footer actions
}

pub enum Reduce { Changed, Ignored, Quit }
```

Return `Changed` when the model changed: the runner rebuilds the page and
publishes only the difference. Return `Ignored` when nothing happened.
`Ignored` on `AppAction::Cancel` quits, so Escape leaves a top-level App.

## Actions you receive

```rust
pub enum AppAction {
    Submit { input: String, text: String },            // Enter in the header Input
    Activate { item: String, action: String },         // a row's activate action
    Delete { item: String },                           // a row's delete action
    Select { item: String },                           // selection moved (list has a select action)
    Toggle { item: String, control: String, on: bool },// a Toggle or Checkmark in a row
    Command { action: String },                        // a footer action
    Back,                                              // the Page's back action
    Cancel,                                            // Escape without a back action
    Change { node: String, action: String, value: AppValue }, // anything else, raw
}
```

Every field is an id you chose in `page()`. Terminal keys, mouse clicks, and
hosted renderer events all arrive through the same enum, so a reducer is
written once.

## Components

- `Page` — one screen: title, optional back action, optional `Input`
  header, one body, footer actions.
- `List` / `ListItem` — the standard body. Rows have a label, optional
  `detail`, right-aligned `value`, `leading` / `trailing` / `accessory`
  slots, `top` / `bottom` bands, an optional media column, and actions.
- `Toggle`, `Checkmark`, `Badge`, `StatusSymbol`, `Disclosure` — the closed
  set of row slots.
- `ListItem::divider(id)` — a passive separator row navigation skips.
- `Gauge`, `Sparkline`, `BarChart`, `LineChart` — a Page body or a row band
  (`ListItemBand::gauge(..)`) or compact trailing slot.
- `Content` — read-only styled lines for detail screens (`Page::new(title,
  Content::new(..))`), with a back action to return.
- `FooterAction` — bottom commands with optional accelerator keys and a
  `busy(true)` spinner.
- `TextBox` — a multi-line input for prompts and forms (used directly with
  Ratatui today, not through `Page`).

Rows stack automatically on narrow panes with
`List::row_layout(ListRowLayout::Auto { stack_below_width: 60 })`.

## Three rules

1. **Ids are stable.** Give every row, control, and input an id derived from
   your model (`todo-42`, `todo-42-toggle`). The runner diffs pages by id,
   and renderers keep selection and focus across changes by id.
2. **Actions are named.** Every clickable thing declares an action id in
   `page()` (`.activate_action("open")`, `.submit_action("add")`). Nothing is
   clickable without one, and that name comes back in `AppAction`.
3. **No layout code.** `page()` describes what is on screen, never where.
   The terminal, SwiftUI, and web each lay it out in their own idiom. If you
   are computing widths or rows in `page()`, stop.

For the wire protocol, hosted rendering, and renderer lifecycle see
[`ui-components.md`](ui-components.md). You do not need it to write an App.
