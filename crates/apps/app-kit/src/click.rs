//! Target-aware mouse click sequencing for terminal Apps.

use std::time::{Duration, Instant};

/// Default maximum gap between the two presses of a double click.
pub const DEFAULT_DOUBLE_CLICK_INTERVAL: Duration = Duration::from_millis(500);

/// Detects two timely clicks on the same logical item.
///
/// The target should identify the item rather than its screen position. That
/// keeps selection redraws harmless while preventing two different rows from
/// being interpreted as a double click. A completed double click resets the
/// sequence.
#[derive(Clone, Debug)]
pub struct DoubleClickTracker<T> {
    interval: Duration,
    previous: Option<(T, Instant)>,
}

impl<T> DoubleClickTracker<T> {
    #[must_use]
    pub const fn new() -> Self {
        Self::with_interval(DEFAULT_DOUBLE_CLICK_INTERVAL)
    }

    #[must_use]
    pub const fn with_interval(interval: Duration) -> Self {
        Self {
            interval,
            previous: None,
        }
    }

    /// Forgets an incomplete click sequence.
    pub fn reset(&mut self) {
        self.previous = None;
    }
}

impl<T> Default for DoubleClickTracker<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: PartialEq> DoubleClickTracker<T> {
    /// Records a click and returns `true` when it completes a double click.
    #[must_use]
    pub fn click(&mut self, target: T) -> bool {
        self.click_at(target, Instant::now())
    }

    fn click_at(&mut self, target: T, now: Instant) -> bool {
        let completed = self.previous.as_ref().is_some_and(|(previous, at)| {
            previous == &target
                && now
                    .checked_duration_since(*at)
                    .is_some_and(|elapsed| elapsed <= self.interval)
        });
        self.previous = (!completed).then_some((target, now));
        completed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_timely_clicks_on_the_same_target_complete_once() {
        let start = Instant::now();
        let mut clicks = DoubleClickTracker::new();

        assert!(!clicks.click_at("row-a", start));
        assert!(clicks.click_at("row-a", start + Duration::from_millis(300)));
        assert!(!clicks.click_at("row-a", start + Duration::from_millis(350)));
    }

    #[test]
    fn another_target_or_an_expired_click_rearms_the_sequence() {
        let start = Instant::now();
        let mut clicks = DoubleClickTracker::new();

        assert!(!clicks.click_at(1, start));
        assert!(!clicks.click_at(2, start + Duration::from_millis(100)));
        assert!(!clicks.click_at(2, start + Duration::from_millis(700)));
        assert!(clicks.click_at(2, start + Duration::from_millis(800)));
    }

    #[test]
    fn reset_cancels_a_pending_double_click() {
        let start = Instant::now();
        let mut clicks = DoubleClickTracker::new();

        assert!(!clicks.click_at("row", start));
        clicks.reset();
        assert!(!clicks.click_at("row", start + Duration::from_millis(100)));
    }
}
