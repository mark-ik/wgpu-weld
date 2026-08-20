//! Cookie reads and writes through CEF's global cookie manager.
//!
//! Every one of these is asynchronous in CEF: reads arrive one cookie at a
//! time through a visitor, and writes report completion on a callback. So the
//! host-facing shape is request-then-poll rather than a blocking getter.
//!
//! A blocking getter is not merely unidiomatic here, it deadlocks. On Linux and
//! macOS the host thread *is* CEF's UI thread, so waiting on it stops the loop
//! that would deliver the answer. `scrying` settled on request/poll for the
//! same reason, and matching its shape is deliberate.

use std::sync::Mutex;

use crate::surface::Cookie;
#[cfg(feature = "cef-runtime")]
use crate::surface::SameSite;

/// Collects a visitor's cookies until the read finishes, then hands the batch
/// to the host exactly once.
#[derive(Debug, Default)]
pub(crate) struct CookieJar {
    building: Mutex<Vec<Cookie>>,
    ready: Mutex<Option<Vec<Cookie>>>,
}

impl CookieJar {
    pub(crate) fn push(&self, cookie: Cookie) {
        self.building.lock().unwrap().push(cookie);
    }

    /// Publish whatever has been collected as the answer.
    pub(crate) fn finish(&self) {
        let batch = std::mem::take(&mut *self.building.lock().unwrap());
        *self.ready.lock().unwrap() = Some(batch);
    }

    pub(crate) fn take(&self) -> Option<Vec<Cookie>> {
        self.ready.lock().unwrap().take()
    }

    /// Drop anything half-collected. Called when a new read starts so a
    /// previous partial visit cannot leak into it.
    pub(crate) fn reset(&self) {
        self.building.lock().unwrap().clear();
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
struct FinishGuard(std::sync::Arc<CookieJar>);

impl Drop for FinishGuard {
    fn drop(&mut self) {
        self.0.finish();
    }
}

impl FinishOnDrop {
    pub(crate) fn new(jar: std::sync::Arc<CookieJar>) -> Self {
        Self(std::sync::Arc::new(FinishGuard(jar)))
    }

    pub(crate) fn jar(&self) -> &CookieJar {
        &self.0.0
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

    fn cookie(name: &str) -> Cookie {
        Cookie {
            name: name.into(),
            ..Default::default()
        }
    }

    #[test]
    fn a_batch_is_handed_over_once() {
        let jar = CookieJar::default();
        assert_eq!(jar.take(), None, "nothing was requested yet");
        jar.push(cookie("a"));
        jar.push(cookie("b"));
        assert_eq!(jar.take(), None, "an unfinished visit must not be readable");

        jar.finish();
        let batch = jar.take().expect("finished visit should be readable");
        assert_eq!(batch.len(), 2);
        assert_eq!(jar.take(), None, "a batch is delivered exactly once");
    }

    #[test]
    fn a_new_read_does_not_inherit_a_partial_one() {
        let jar = CookieJar::default();
        jar.push(cookie("stale"));
        jar.reset();
        jar.push(cookie("fresh"));
        jar.finish();
        let batch = jar.take().unwrap();
        assert_eq!(batch.len(), 1);
        assert_eq!(batch[0].name, "fresh");
    }

    #[test]
    fn an_empty_result_is_still_an_answer() {
        // "no cookies" and "not answered yet" must not look the same to a host.
        let jar = CookieJar::default();
        jar.finish();
        assert_eq!(jar.take(), Some(Vec::new()));
    }

    #[test]
    fn dropping_the_visitor_publishes_the_answer() {
        // The case that bit in practice: a store with no cookies never calls
        // the visitor, so only its destruction can end the read.
        let jar = std::sync::Arc::new(CookieJar::default());
        let guard = FinishOnDrop::new(jar.clone());
        let copy = guard.clone();
        assert_eq!(jar.take(), None);
        drop(guard);
        assert_eq!(
            jar.take(),
            None,
            "a surviving handle means the read is still open"
        );
        drop(copy);
        assert_eq!(jar.take(), Some(Vec::new()));
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
        pub(crate) fn build(jar: Arc<CookieJar>) -> cef::CookieVisitor {
            Self::new(FinishOnDrop::new(jar))
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
    url: Option<&str>,
) -> Result<(), crate::error::WeldError> {
    use cef::ImplCookieManager;

    let manager = manager(browser)?;
    jar.reset();
    let mut visitor = cef_backed::WeldCookieVisitor::build(jar.clone());

    let accepted = match url {
        Some(url) => {
            let url: cef::CefString = url.into();
            manager.visit_url_cookies(Some(&url), 1, Some(&mut visitor))
        }
        None => manager.visit_all_cookies(Some(&mut visitor)),
    };
    if accepted == 0 {
        // CEF refuses when the store cannot be read at all. Report an empty
        // answer rather than leaving the host polling forever.
        jar.finish();
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
