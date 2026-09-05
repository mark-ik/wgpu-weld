// Copyright 2026 Mark Alan Boykin
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// SPDX-License-Identifier: MPL-2.0

//! Cookie reads and writes through CEF's global cookie manager.
//!
//! Every one of these is asynchronous in CEF: reads arrive one cookie at a
//! time through a visitor, and writes report completion on a callback. So the
//! host-facing shape is request-then-event rather than a blocking getter.
//!
//! A blocking getter is not merely unidiomatic here, it deadlocks. On Linux and
//! macOS the host thread *is* CEF's UI thread, so waiting on it stops the loop
//! that would deliver the answer. `scrying` settled on request/poll for the
//! same reason.

use std::sync::Mutex;

#[cfg(feature = "cef-runtime")]
use crate::surface::SameSite;
use crate::surface::{CefSurfaceEvent, Cookie, WebEventQueue, WebRequestId};

/// Collects a visitor's cookies until the read finishes, then hands the batch
/// to the host exactly once.
#[derive(Debug)]
pub(crate) struct CookieJar {
    state: Mutex<Option<(WebRequestId, Vec<Cookie>)>>,
    events: std::sync::Arc<WebEventQueue>,
}

impl CookieJar {
    pub(crate) fn new(events: std::sync::Arc<WebEventQueue>) -> Self {
        Self {
            state: Mutex::new(None),
            events,
        }
    }

    pub(crate) fn begin(&self, id: WebRequestId) -> Result<(), crate::error::WeldError> {
        let mut state = self.state.lock().unwrap();
        if state.is_some() {
            return Err(crate::error::WeldError::BrowserOp(
                "a cookie read is already in flight".into(),
            ));
        }
        *state = Some((id, Vec::new()));
        Ok(())
    }

    pub(crate) fn push(&self, cookie: Cookie) {
        if let Some((_, batch)) = self.state.lock().unwrap().as_mut() {
            batch.push(cookie);
        }
    }

    /// Publish whatever has been collected as the answer.
    pub(crate) fn finish(&self, id: WebRequestId) {
        let completion = {
            let mut state = self.state.lock().unwrap();
            match state.take() {
                Some((active_id, batch)) if active_id == id => Some(batch),
                Some(active) => {
                    *state = Some(active);
                    None
                }
                None => None,
            }
        };
        if let Some(batch) = completion {
            self.events.push(CefSurfaceEvent::CookiesCompleted {
                id,
                result: Ok(batch),
            });
        }
    }

    pub(crate) fn abort(&self, id: WebRequestId) {
        let mut state = self.state.lock().unwrap();
        if state
            .as_ref()
            .is_some_and(|(active_id, _)| *active_id == id)
        {
            *state = None;
        }
    }

    pub(crate) fn fail_active(&self, reason: &str) {
        let active = self.state.lock().unwrap().take();
        if let Some((id, _)) = active {
            self.events.push(CefSurfaceEvent::CookiesCompleted {
                id,
                result: Err(reason.to_owned()),
            });
        }
    }
}

/// Publishes the batch when the visitor is destroyed.
///
/// CEF calls the visitor once per cookie and never at all when there are none,
/// so completion cannot be detected from the callbacks alone: a store with no
/// cookies would leave the host polling forever, unable to tell "none" from
/// "not yet". CEF does release the visitor when the walk ends either way, so
/// that release is the signal. This lives in a field of the visitor rather
/// than in a `Drop` on the macro-generated type, which owns its own lifecycle.
/// Cloneable, because CEF's wrapper type is: the answer is published when the
/// *last* handle goes, not the first.
#[derive(Debug, Clone)]
pub(crate) struct FinishOnDrop(std::sync::Arc<FinishGuard>);

#[derive(Debug)]
struct FinishGuard {
    jar: std::sync::Arc<CookieJar>,
    id: WebRequestId,
    armed: std::sync::atomic::AtomicBool,
}

impl Drop for FinishGuard {
    fn drop(&mut self) {
        if self.armed.load(std::sync::atomic::Ordering::Acquire) {
            self.jar.finish(self.id);
        }
    }
}

impl FinishOnDrop {
    pub(crate) fn new(jar: std::sync::Arc<CookieJar>, id: WebRequestId) -> Self {
        Self(std::sync::Arc::new(FinishGuard {
            jar,
            id,
            armed: std::sync::atomic::AtomicBool::new(true),
        }))
    }

    pub(crate) fn jar(&self) -> &CookieJar {
        &self.0.jar
    }

    pub(crate) fn disarm(&self) {
        self.0
            .armed
            .store(false, std::sync::atomic::Ordering::Release);
    }
}

#[cfg(feature = "cef-runtime")]
pub(crate) fn from_cef(c: &cef::Cookie) -> Cookie {
    Cookie {
        name: c.name.to_string(),
        value: c.value.to_string(),
        domain: c.domain.to_string(),
        path: c.path.to_string(),
        secure: c.secure != 0,
        http_only: c.httponly != 0,
        same_site: match c.same_site {
            cef::CookieSameSite::STRICT_MODE => Some(SameSite::Strict),
            cef::CookieSameSite::LAX_MODE => Some(SameSite::Lax),
            cef::CookieSameSite::NO_RESTRICTION => Some(SameSite::None),
            _ => None,
        },
        // CEF carries times as basetime microseconds; expose seconds so a host
        // does not have to know that.
        expires: (c.has_expires != 0).then(|| c.expires.val as f64 / 1_000_000.0),
        partitioned: false,
    }
}

#[cfg(feature = "cef-runtime")]
pub(crate) fn to_cef(c: &Cookie) -> cef::Cookie {
    cef::Cookie {
        name: c.name.as_str().into(),
        value: c.value.as_str().into(),
        domain: c.domain.as_str().into(),
        path: c.path.as_str().into(),
        secure: c.secure as _,
        httponly: c.http_only as _,
        has_expires: c.expires.is_some() as _,
        expires: cef::Basetime {
            val: c.expires.map(|e| (e * 1_000_000.0) as i64).unwrap_or(0),
        },
        same_site: match c.same_site {
            Some(SameSite::Strict) => cef::CookieSameSite::STRICT_MODE,
            Some(SameSite::Lax) => cef::CookieSameSite::LAX_MODE,
            Some(SameSite::None) => cef::CookieSameSite::NO_RESTRICTION,
            None => cef::CookieSameSite::UNSPECIFIED,
        },
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jar() -> (std::sync::Arc<CookieJar>, std::sync::Arc<WebEventQueue>) {
        let events = std::sync::Arc::new(WebEventQueue::default());
        (std::sync::Arc::new(CookieJar::new(events.clone())), events)
    }

    fn completed(events: &WebEventQueue) -> Option<(WebRequestId, Vec<Cookie>)> {
        match events.poll()? {
            CefSurfaceEvent::CookiesCompleted {
                id,
                result: Ok(cookies),
            } => Some((id, cookies)),
            other => panic!("unexpected event: {other:?}"),
        }
    }

    fn cookie(name: &str) -> Cookie {
        Cookie {
            name: name.into(),
            ..Default::default()
        }
    }

    #[test]
    fn a_batch_is_handed_over_once() {
        let (jar, events) = jar();
        let id = WebRequestId::new(7);
        jar.begin(id).unwrap();
        jar.push(cookie("a"));
        jar.push(cookie("b"));
        assert!(events.poll().is_none(), "an unfinished visit must not emit");

        jar.finish(id);
        let (completed_id, batch) = completed(&events).unwrap();
        assert_eq!(completed_id, id);
        assert_eq!(batch.len(), 2);
        assert!(events.poll().is_none(), "a batch is delivered exactly once");
    }

    #[test]
    fn a_second_read_is_rejected_until_the_first_settles() {
        let (jar, events) = jar();
        let first = WebRequestId::new(1);
        jar.begin(first).unwrap();
        assert!(jar.begin(WebRequestId::new(2)).is_err());
        jar.finish(first);
        assert_eq!(completed(&events).unwrap().0, first);
        jar.begin(WebRequestId::new(2)).unwrap();
    }

    #[test]
    fn an_empty_result_is_still_an_answer() {
        // "no cookies" and "not answered yet" must not look the same to a host.
        let (jar, events) = jar();
        let id = WebRequestId::new(9);
        jar.begin(id).unwrap();
        jar.finish(id);
        assert_eq!(completed(&events), Some((id, Vec::new())));
    }

    #[test]
    fn dropping_the_visitor_publishes_the_answer() {
        // The case that bit in practice: a store with no cookies never calls
        // the visitor, so only its destruction can end the read.
        let (jar, events) = jar();
        let id = WebRequestId::new(11);
        jar.begin(id).unwrap();
        let guard = FinishOnDrop::new(jar.clone(), id);
        let copy = guard.clone();
        drop(guard);
        assert!(
            events.poll().is_none(),
            "a surviving handle keeps the read open"
        );
        drop(copy);
        assert_eq!(completed(&events), Some((id, Vec::new())));
    }

    #[test]
    fn a_refused_visit_emits_no_completion() {
        let (jar, events) = jar();
        let id = WebRequestId::new(13);
        jar.begin(id).unwrap();
        let guard = FinishOnDrop::new(jar.clone(), id);
        guard.disarm();
        jar.abort(id);
        drop(guard);
        assert!(events.poll().is_none());
        jar.begin(WebRequestId::new(14)).unwrap();
    }

    #[test]
    fn closing_settles_the_active_read_once() {
        let (jar, events) = jar();
        let id = WebRequestId::new(15);
        jar.begin(id).unwrap();
        jar.fail_active("browser closed");
        jar.finish(id);
        assert!(matches!(
            events.poll(),
            Some(CefSurfaceEvent::CookiesCompleted { id: seen, result: Err(reason) })
                if seen == id && reason == "browser closed"
        ));
        assert!(events.poll().is_none());
    }
}

// ── CEF-side plumbing ────────────────────────────────────────────────────────

#[cfg(feature = "cef-runtime")]
mod cef_backed {
    use super::*;
    use cef::*;
    use std::sync::Arc;

    cef::wrap_cookie_visitor! {
        pub(crate) struct WeldCookieVisitor {
            jar: FinishOnDrop,
        }

        impl CookieVisitor {
            fn visit(
                &self,
                cookie: Option<&cef::Cookie>,
                count: ::std::os::raw::c_int,
                total: ::std::os::raw::c_int,
                _delete_cookie: Option<&mut ::std::os::raw::c_int>,
            ) -> ::std::os::raw::c_int {
                let _ = (count, total);
                if let Some(cookie) = cookie {
                    self.jar.jar().push(super::from_cef(cookie));
                }
                // Completion is signalled by the visitor being dropped, not by
                // counting: see FinishOnDrop.
                1 // keep visiting
            }
        }
    }

    impl WeldCookieVisitor {
        pub(crate) fn build(
            jar: Arc<CookieJar>,
            id: WebRequestId,
        ) -> (cef::CookieVisitor, FinishOnDrop) {
            let guard = FinishOnDrop::new(jar, id);
            (Self::new(guard.clone()), guard)
        }
    }
}

/// Start a cookie read. `url` of `None` reads every cookie in the store.
///
/// The answer arrives later through [`CookieJar::take`]; an immediate `Ok`
/// only means CEF accepted the request.
#[cfg(feature = "cef-runtime")]
pub(crate) fn request(
    browser: &cef::Browser,
    jar: &std::sync::Arc<CookieJar>,
    id: WebRequestId,
    url: Option<&str>,
) -> Result<(), crate::error::WeldError> {
    use cef::ImplCookieManager;

    let manager = manager(browser)?;
    jar.begin(id)?;
    let (mut visitor, guard) = cef_backed::WeldCookieVisitor::build(jar.clone(), id);

    let accepted = match url {
        Some(url) => {
            let url: cef::CefString = url.into();
            manager.visit_url_cookies(Some(&url), 1, Some(&mut visitor))
        }
        None => manager.visit_all_cookies(Some(&mut visitor)),
    };
    if accepted == 0 {
        guard.disarm();
        jar.abort(id);
        return Err(crate::error::WeldError::BrowserOp(
            "CEF refused the cookie visit".into(),
        ));
    }
    Ok(())
}

#[cfg(feature = "cef-runtime")]
pub(crate) fn set(
    browser: &cef::Browser,
    url: &str,
    cookie: &Cookie,
) -> Result<(), crate::error::WeldError> {
    use cef::ImplCookieManager;

    let manager = manager(browser)?;
    let url_s: cef::CefString = url.into();
    let cef_cookie = to_cef(cookie);
    if manager.set_cookie(Some(&url_s), Some(&cef_cookie), None) == 0 {
        return Err(crate::error::WeldError::BrowserOp(format!(
            "CEF rejected the cookie for {url}; check the domain and path against the URL"
        )));
    }
    Ok(())
}

#[cfg(feature = "cef-runtime")]
pub(crate) fn delete(
    browser: &cef::Browser,
    url: Option<&str>,
    name: Option<&str>,
) -> Result<(), crate::error::WeldError> {
    use cef::ImplCookieManager;

    let manager = manager(browser)?;
    let url_s = url.map(cef::CefString::from);
    let name_s = name.map(cef::CefString::from);
    if manager.delete_cookies(url_s.as_ref(), name_s.as_ref(), None) == 0 {
        return Err(crate::error::WeldError::BrowserOp(
            "CEF refused the cookie delete".into(),
        ));
    }
    Ok(())
}

/// Return this browser's cookie manager, never CEF's process-global manager.
///
/// Every CEF producer receives a distinct RequestContext. Going through the
/// browser host is what keeps the public cookie API in the same profile as the
/// browser's navigation, local storage, cache, and permissions.
#[cfg(feature = "cef-runtime")]
fn manager(browser: &cef::Browser) -> Result<cef::CookieManager, crate::error::WeldError> {
    use cef::{ImplBrowser, ImplBrowserHost, ImplRequestContext};

    let host = browser.host().ok_or_else(|| {
        crate::error::WeldError::BrowserOp("browser has no CEF host for its cookie context".into())
    })?;
    let context = host.request_context().ok_or_else(|| {
        crate::error::WeldError::BrowserOp("browser has no CEF request context for cookies".into())
    })?;
    context.cookie_manager(None).ok_or_else(|| {
        crate::error::WeldError::BrowserOp("CEF request context has no cookie manager".into())
    })
}
