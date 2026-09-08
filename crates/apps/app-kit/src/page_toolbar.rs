//! Renderer-local dropdown state for the shared Page toolbar.
use crate::{MenuTheme, PageToolbar, PopupMenu};
use crossterm::event::{Event, KeyCode, KeyEventKind, MouseButton, MouseEventKind};
use ratatui::{
    Frame,
    layout::{Position, Rect},
};

#[derive(Default)]
pub struct PageToolbarState {
    popup: Option<(PageToolbar, PopupMenu<String>)>,
}

impl PageToolbarState {
    pub fn is_open(&self) -> bool {
        self.popup.is_some()
    }

    /// `Some` consumes the event; its value is the activated (id, action).
    pub fn handle(
        &mut self,
        event: &Event,
        toolbar: Option<&PageToolbar>,
        title: Rect,
        theme: MenuTheme,
    ) -> Option<Option<(String, String)>> {
        if self
            .popup
            .as_ref()
            .is_some_and(|(spec, _)| Some(spec) != toolbar)
        {
            self.popup = None;
        }
        let toolbar = toolbar?;
        if let Some((_, popup)) = &mut self.popup {
            let mut activate = false;
            match event {
                Event::Key(key) if key.kind != KeyEventKind::Release => match key.code {
                    KeyCode::Esc => {
                        self.popup = None;
                        return Some(None);
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        popup.move_selection(-1);
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        popup.move_selection(1);
                    }
                    KeyCode::Enter | KeyCode::Char(' ') => activate = true,
                    _ => {}
                },
                Event::Mouse(mouse) => {
                    popup.track_mouse(mouse);
                    if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
                        if popup.action_index_for_mouse(mouse).is_some() {
                            activate = true;
                        } else {
                            self.popup = None;
                        }
                    }
                }
                Event::Resize(_, _) => {
                    self.popup = None;
                    return None;
                }
                _ => {}
            }
            if activate {
                let (_, popup) = self.popup.take()?;
                let id = popup.selected_value()?;
                let item = toolbar
                    .menu
                    .as_ref()?
                    .items
                    .iter()
                    .find(|item| &item.id == id && !item.disabled)?;
                return Some(Some((item.id.clone(), item.action.clone())));
            }
            return Some(None);
        }
        let (primary, menu) = toolbar.areas(title);
        match event {
            Event::Key(key) if key.code == KeyCode::F(10) && key.kind != KeyEventKind::Release => {
                let spec = toolbar.menu.as_ref()?;
                if spec.items.iter().any(|item| !item.disabled) {
                    self.popup = Some((
                        toolbar.clone(),
                        spec.popup(Position::new(primary.x, title.y + 1), theme),
                    ));
                }
                Some(None)
            }
            Event::Key(key)
                if key.kind != KeyEventKind::Release
                    && toolbar.primary.matches_key(key)
                    && !toolbar.primary.busy =>
            {
                Some(Some((
                    toolbar.primary.id.clone(),
                    toolbar.primary.action.clone(),
                )))
            }
            Event::Mouse(mouse) if mouse.kind == MouseEventKind::Down(MouseButton::Left) => {
                let position = Position::new(mouse.column, mouse.row);
                if menu.contains(position) {
                    let spec = toolbar.menu.as_ref()?;
                    if spec.items.iter().any(|item| !item.disabled) {
                        self.popup = Some((
                            toolbar.clone(),
                            spec.popup(Position::new(primary.x, title.y + 1), theme),
                        ));
                    }
                    Some(None)
                } else if primary.contains(position) {
                    Some(
                        (!toolbar.primary.disabled && !toolbar.primary.busy)
                            .then(|| (toolbar.primary.id.clone(), toolbar.primary.action.clone())),
                    )
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    pub fn render(&mut self, frame: &mut Frame<'_>, toolbar: Option<&PageToolbar>) {
        if self
            .popup
            .as_ref()
            .is_some_and(|(spec, _)| Some(spec) != toolbar)
        {
            self.popup = None;
        }
        if let Some((_, popup)) = &mut self.popup {
            popup.render(frame);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FooterAction, InputField, List, ListState, Page, SemanticMenu, SemanticMenuItem};
    use crossterm::event::{KeyEvent, KeyModifiers, MouseEvent};
    use ratatui::{Terminal, backend::TestBackend};

    fn toolbar() -> PageToolbar {
        PageToolbar::new(FooterAction::new("fetch", "Fetch", "fetch")).menu(SemanticMenu::new(
            "Remote actions",
            [
                SemanticMenuItem::new("pull", "Pull", "pull").disabled(true),
                SemanticMenuItem::new("push", "Push", "push"),
            ],
        ))
    }

    #[test]
    fn toolbar_hit_targets_match_paint_at_narrow_widths_and_disabled_buttons_do_not_activate() {
        for width in [4, 12, 24, 60] {
            let page = Page::new(
                "A long title which must not overlap the action",
                List::new("files", vec![]),
            )
            .toolbar(toolbar());
            page.validate().unwrap();
            assert!(
                page.required_capabilities()
                    .contains(&crate::PAGE_TOOLBAR_CAPABILITY)
            );
            let mut terminal = Terminal::new(TestBackend::new(width, 8)).unwrap();
            let mut input = InputField::new("");
            let mut list = ListState::default();
            terminal
                .draw(|frame| frame.render_widget(page.widget(&mut input, &mut list), frame.area()))
                .unwrap();
            let title = page.layout(Rect::new(0, 0, width, 8)).title;
            let (primary, menu) = page.toolbar.as_ref().unwrap().areas(title);
            assert!(primary.right() <= menu.x && menu.right() <= width);
            if primary.width > 0 {
                let click = Event::Mouse(MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column: primary.x,
                    row: primary.y,
                    modifiers: KeyModifiers::NONE,
                });
                let mut state = PageToolbarState::default();
                assert_eq!(
                    state.handle(&click, page.toolbar.as_ref(), title, MenuTheme::dark()),
                    Some(Some(("fetch".into(), "fetch".into())))
                );
                let mut disabled = toolbar();
                disabled.primary.disabled = true;
                assert_eq!(
                    state.handle(&click, Some(&disabled), title, MenuTheme::dark()),
                    Some(None)
                );
            }
            if width >= 24 {
                let buffer = terminal.backend().buffer();
                let line: String = (primary.x..primary.right())
                    .map(|x| buffer[(x, primary.y)].symbol())
                    .collect();
                assert_eq!(line, " Fetch ");
            }
        }
    }

    #[test]
    fn toolbar_menu_skips_disabled_actions_and_cancels_locally() {
        let toolbar = toolbar();
        let title = Rect::new(0, 0, 44, 2);
        let mut state = PageToolbarState::default();
        let key = |code| Event::Key(KeyEvent::new(code, KeyModifiers::NONE));
        assert_eq!(
            state.handle(
                &key(KeyCode::F(10)),
                Some(&toolbar),
                title,
                MenuTheme::dark()
            ),
            Some(None)
        );
        assert!(state.is_open());
        assert_eq!(
            state.handle(
                &key(KeyCode::Enter),
                Some(&toolbar),
                title,
                MenuTheme::dark()
            ),
            Some(Some(("push".into(), "push".into())))
        );
        assert!(!state.is_open());
        state.handle(
            &key(KeyCode::F(10)),
            Some(&toolbar),
            title,
            MenuTheme::dark(),
        );
        assert_eq!(
            state.handle(&key(KeyCode::Esc), Some(&toolbar), title, MenuTheme::dark()),
            Some(None)
        );
        assert!(!state.is_open());
    }
}
