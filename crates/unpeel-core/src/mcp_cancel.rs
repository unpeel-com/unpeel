//! In-flight cancellation for the stdio MCP server.
//!
//! The stdio reader thread registers a [`CancelToken`] per request id and
//! flips it when the client sends `notifications/cancelled`. The tool worker
//! installs its request's token in a thread-local before dispatch; long poll
//! loops and post-approval effect boundaries call [`bail_if_cancelled`] so a
//! cancelled call unwinds within one poll interval instead of running to its
//! timeout. Per the MCP spec, the worker drops the response of a cancelled
//! request rather than answering it.

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

#[derive(Clone, Default)]
pub(crate) struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub(crate) fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

thread_local! {
    static CURRENT: RefCell<Option<CancelToken>> = const { RefCell::new(None) };
}

/// Install `token` as the current thread's running-request token until the
/// returned guard drops (including on unwind).
pub(crate) fn install(token: CancelToken) -> InstallGuard {
    CURRENT.with(|current| *current.borrow_mut() = Some(token));
    InstallGuard
}

pub(crate) struct InstallGuard;

impl Drop for InstallGuard {
    fn drop(&mut self) {
        CURRENT.with(|current| *current.borrow_mut() = None);
    }
}

pub(crate) fn cancellation_requested() -> bool {
    CURRENT.with(|current| {
        current
            .borrow()
            .as_ref()
            .is_some_and(CancelToken::is_cancelled)
    })
}

/// Error out of a blocking tool call once the client has cancelled it. The
/// worker drops the response either way; the message only reaches the trace.
pub(crate) fn bail_if_cancelled() -> Result<(), String> {
    if cancellation_requested() {
        return Err("Request cancelled by the MCP client (notifications/cancelled).".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_cancel_is_visible_through_clones() {
        let token = CancelToken::default();
        let clone = token.clone();
        assert!(!clone.is_cancelled());
        token.cancel();
        assert!(clone.is_cancelled());
    }

    #[test]
    fn no_installed_token_means_not_cancelled() {
        assert!(!cancellation_requested());
        assert!(bail_if_cancelled().is_ok());
    }

    #[test]
    fn install_guard_scopes_the_current_token() {
        let token = CancelToken::default();
        {
            let _guard = install(token.clone());
            assert!(!cancellation_requested());
            token.cancel();
            assert!(cancellation_requested());
            assert!(bail_if_cancelled().is_err());
        }
        // Guard dropped: the cancelled token no longer applies to this thread.
        assert!(!cancellation_requested());
    }

    #[test]
    fn tokens_are_thread_local() {
        let token = CancelToken::default();
        let _guard = install(token.clone());
        token.cancel();
        let other = std::thread::spawn(cancellation_requested).join().unwrap();
        assert!(!other);
        assert!(cancellation_requested());
    }
}
