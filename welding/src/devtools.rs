//! The Chrome DevTools Protocol, straight through.
//!
//! This is the one thing the CEF lane can offer that a system webview cannot.
//! Rather than wrap CDP in a typed API that would age badly against a protocol
//! Chromium revises every release, `welding` passes the wire format in both
//! directions: JSON in, JSON out, exactly as the protocol documents it, so any
//! existing CDP client can drive it.
//!
//! CDP is chatty — a single `Page.enable` produces a steady stream of events —
//! so the queue is bounded and counts what it drops rather than growing without
//! limit behind a host that has stopped polling.

use std::collections::VecDeque;
use std::sync::Mutex;

/// How many unread protocol messages to keep. Chosen to survive a burst from
/// something like `Page.enable` while a host is busy rendering a frame, not to
/// be a transcript: a host that wants every message must poll every tick.
const MAX_QUEUED: usize = 512;

#[derive(Default)]
pub(crate) struct DevToolsChannel {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    enabled: bool,
    queue: VecDeque<String>,
    dropped: u64,
}

impl DevToolsChannel {
    pub(crate) fn set_enabled(&self, enabled: bool) {
        self.inner.lock().unwrap().enabled = enabled;
    }

    pub(crate) fn is_enabled(&self) -> bool {
        self.inner.lock().unwrap().enabled
    }

    /// Queue one protocol message, dropping the oldest if the host is behind.
    pub(crate) fn push(&self, message: String) {
        let mut inner = self.inner.lock().unwrap();
        if inner.queue.len() >= MAX_QUEUED {
            inner.queue.pop_front();
            inner.dropped += 1;
        }
        inner.queue.push_back(message);
    }

    pub(crate) fn pop(&self) -> Option<String> {
        self.inner.lock().unwrap().queue.pop_front()
    }

    /// How many messages have been dropped for want of polling. Non-zero means
    /// the host is not keeping up and its view of the protocol has gaps.
    pub(crate) fn dropped(&self) -> u64 {
        self.inner.lock().unwrap().dropped
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_protocol_is_off_until_asked_for() {
        let c = DevToolsChannel::default();
        assert!(
            !c.is_enabled(),
            "CDP is chatty; do not subscribe by default"
        );
        c.set_enabled(true);
        assert!(c.is_enabled());
    }

    #[test]
    fn messages_come_back_in_order() {
        let c = DevToolsChannel::default();
        c.push("first".into());
        c.push("second".into());
        assert_eq!(c.pop().as_deref(), Some("first"));
        assert_eq!(c.pop().as_deref(), Some("second"));
        assert_eq!(c.pop(), None);
    }

    #[test]
    fn a_host_that_stops_polling_loses_the_oldest_and_is_told() {
        let c = DevToolsChannel::default();
        for i in 0..MAX_QUEUED + 10 {
            c.push(format!("m{i}"));
        }
        assert_eq!(c.dropped(), 10, "drops must be counted, not silent");
        // The newest survive; the oldest are the ones gone.
        assert_eq!(c.pop().as_deref(), Some("m10"));
    }

    #[test]
    fn nothing_is_dropped_while_the_host_keeps_up() {
        let c = DevToolsChannel::default();
        for i in 0..MAX_QUEUED * 3 {
            c.push(format!("m{i}"));
            assert_eq!(c.pop(), Some(format!("m{i}")));
        }
        assert_eq!(c.dropped(), 0);
    }
}
