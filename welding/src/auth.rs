// Copyright 2026 Mark Alan Boykin
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// SPDX-License-Identifier: MPL-2.0

//! HTTP authentication challenges.
//!
//! A server (or a proxy) demands credentials and CEF asks the host for them.
//! Unlike the download destination, this one *can* be answered later: CEF's
//! auth callback is reference-counted and documented as answerable
//! asynchronously, so the challenge is held and the host replies on its own
//! schedule with [`crate::CefSurfaceProducer::answer_auth`].
//!
//! Holding it has a cost, which is why answering is opt-in. An unanswered
//! challenge keeps the request open indefinitely, so a host that never wired
//! the event up would hang every authenticated request instead of failing it.
//! With `handle_auth_challenges` left at its default, `welding` reports the
//! challenge and immediately declines it, which produces an ordinary
//! authentication failure the page can render.
//!
//! Nothing here logs a username or a password, and no event carries one. The
//! credentials the host supplies go straight to CEF.

#[cfg(feature = "cef-runtime")]
use std::collections::HashMap;
use std::sync::Mutex;

/// Identifies one auth challenge until it is answered.
pub type AuthId = u32;

#[derive(Default)]
pub(crate) struct AuthChallenges {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    next_id: AuthId,
    /// Whether the host asked to answer challenges itself.
    enabled: bool,
    #[cfg(feature = "cef-runtime")]
    pending: HashMap<AuthId, cef::AuthCallback>,
}

impl AuthChallenges {
    pub(crate) fn set_enabled(&self, enabled: bool) {
        self.inner.lock().unwrap().enabled = enabled;
    }

    pub(crate) fn is_enabled(&self) -> bool {
        self.inner.lock().unwrap().enabled
    }

    /// Allocate the id the host will answer with.
    pub(crate) fn next_id(&self) -> AuthId {
        let mut inner = self.inner.lock().unwrap();
        inner.next_id = inner.next_id.wrapping_add(1);
        inner.next_id
    }

    /// Hold a challenge until the host answers it.
    #[cfg(feature = "cef-runtime")]
    pub(crate) fn hold(&self, id: AuthId, callback: cef::AuthCallback) {
        self.inner.lock().unwrap().pending.insert(id, callback);
    }

    #[cfg(feature = "cef-runtime")]
    pub(crate) fn take(&self, id: AuthId) -> Option<cef::AuthCallback> {
        self.inner.lock().unwrap().pending.remove(&id)
    }

    /// How many challenges are waiting on the host. A number that only grows
    /// means the host is being told about challenges and never answering.
    // Diagnostic accessor: exercised by unit tests today, kept for host-side
    // leak checks.
    #[allow(dead_code)]
    pub(crate) fn outstanding(&self) -> usize {
        #[cfg(feature = "cef-runtime")]
        {
            return self.inner.lock().unwrap().pending.len();
        }
        #[cfg(not(feature = "cef-runtime"))]
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answering_is_off_until_the_host_asks_for_it() {
        let a = AuthChallenges::default();
        assert!(
            !a.is_enabled(),
            "a host that never wired auth up must not be left holding requests open"
        );
        a.set_enabled(true);
        assert!(a.is_enabled());
    }

    #[test]
    fn ids_are_unique_per_challenge() {
        let a = AuthChallenges::default();
        let ids: Vec<_> = (0..4).map(|_| a.next_id()).collect();
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), ids.len(), "ids repeated: {ids:?}");
    }

    #[test]
    fn zero_is_never_handed_out() {
        // 0 reads like "no challenge" in host code, so it is skipped.
        let a = AuthChallenges::default();
        assert_ne!(a.next_id(), 0);
    }

    #[test]
    fn nothing_is_outstanding_before_a_challenge() {
        assert_eq!(AuthChallenges::default().outstanding(), 0);
    }
}
