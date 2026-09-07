/// A lightweight view stack for ordinary Ratatui Apps.
///
/// `Navigator` owns transitions only; routes remain App-defined values and
/// rendering remains normal Ratatui code. Keeping the root route in the stack
/// makes the common Escape contract explicit: [`Self::back`] returns `false`
/// at the root, where the App may decide whether to stay open or exit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Navigator<Route> {
    stack: Vec<Route>,
}

impl<Route> Navigator<Route> {
    /// Starts navigation at a root route that can never be popped.
    #[must_use]
    pub fn new(root: Route) -> Self {
        Self { stack: vec![root] }
    }

    /// Currently visible route.
    #[must_use]
    pub fn current(&self) -> &Route {
        self.stack
            .last()
            .expect("navigator always retains its root")
    }

    /// Currently visible route, for view-local state updates.
    #[must_use]
    pub fn current_mut(&mut self) -> &mut Route {
        self.stack
            .last_mut()
            .expect("navigator always retains its root")
    }

    /// Number of routes including the root.
    #[must_use]
    pub fn depth(&self) -> usize {
        self.stack.len()
    }

    /// Whether Escape/back has a route to return to.
    #[must_use]
    pub fn can_back(&self) -> bool {
        self.stack.len() > 1
    }

    /// Opens a child/detail route.
    pub fn push(&mut self, route: Route) {
        self.stack.push(route);
    }

    /// Replaces the visible route without changing stack depth.
    pub fn replace(&mut self, route: Route) -> Route {
        std::mem::replace(
            self.stack
                .last_mut()
                .expect("navigator always retains its root"),
            route,
        )
    }

    /// Returns to the previous route. `false` means the root is already open.
    #[must_use]
    pub fn back(&mut self) -> bool {
        if !self.can_back() {
            return false;
        }
        self.stack.pop();
        true
    }

    /// Drops every child route and returns whether navigation changed.
    #[must_use]
    pub fn pop_to_root(&mut self) -> bool {
        if !self.can_back() {
            return false;
        }
        self.stack.truncate(1);
        true
    }

    /// Replaces the entire history with a new root, useful when project or
    /// worktree context changes underneath an App.
    pub fn reset(&mut self, root: Route) {
        self.stack.clear();
        self.stack.push(root);
    }
}

impl<Route: Default> Default for Navigator<Route> {
    fn default() -> Self {
        Self::new(Route::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Debug, Default, PartialEq, Eq)]
    enum Route {
        #[default]
        List,
        Detail(u64),
        Settings,
    }

    #[test]
    fn back_never_pops_the_root() {
        let mut navigator = Navigator::new(Route::List);
        assert!(!navigator.back());
        navigator.push(Route::Detail(42));
        navigator.push(Route::Settings);
        assert_eq!(navigator.depth(), 3);
        assert!(navigator.back());
        assert_eq!(navigator.current(), &Route::Detail(42));
        assert!(navigator.pop_to_root());
        assert_eq!(navigator.current(), &Route::List);
        assert!(!navigator.pop_to_root());
    }

    #[test]
    fn replace_and_reset_make_context_transitions_explicit() {
        let mut navigator = Navigator::default();
        navigator.push(Route::Detail(1));
        assert_eq!(navigator.replace(Route::Detail(2)), Route::Detail(1));
        navigator.reset(Route::Detail(99));
        assert_eq!(navigator.depth(), 1);
        assert_eq!(navigator.current(), &Route::Detail(99));
    }
}
