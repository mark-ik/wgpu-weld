//! One-shot PNG captures over CEF's typed DevTools callback.
//!
//! CEF has no direct windowless-browser screenshot API. `Page.captureScreenshot`
//! is Chromium's supported compositor capture path, and
//! `ExecuteDevToolsMethod` gives it a message id whose answer arrives through
//! `on_dev_tools_method_result`. Keeping that private avoids making a host turn
//! a thumbnail into an ad-hoc CDP client, while keeping the public CDP channel
//! free of welding's own control messages.

use std::{
    collections::{HashSet, VecDeque},
    sync::Mutex,
};

use base64::{Engine as _, engine::general_purpose::STANDARD};

use crate::WeldError;

const MAX_RESULTS: usize = 16;

#[derive(Default)]
pub(crate) struct SnapshotChannel {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    waiting: HashSet<i32>,
    results: VecDeque<Result<Vec<u8>, WeldError>>,
}

impl SnapshotChannel {
    pub(crate) fn begin(&self, id: i32) -> Result<(), WeldError> {
        let mut inner = self.inner.lock().unwrap();
        if !inner.waiting.insert(id) {
            return Err(WeldError::BrowserOp(format!(
                "snapshot request id {id} is already pending"
            )));
        }
        Ok(())
    }

    pub(crate) fn complete(&self, id: i32, success: bool, result: Option<&[u8]>) {
        let mut inner = self.inner.lock().unwrap();
        if !inner.waiting.remove(&id) {
            return;
        }

        let decoded = if !success {
            Err(WeldError::BrowserOp(format!(
                "Page.captureScreenshot failed: {}",
                result
                    .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
                    .unwrap_or_else(|| "CEF supplied no error payload".into())
            )))
        } else {
            decode_png(result)
        };

        if inner.results.len() == MAX_RESULTS {
            // Snapshot requests are explicitly one-shot and their queue is
            // deliberately small. If a host starts more than it reads, keep
            // the most recent preview rather than retaining unbounded PNGs.
            inner.results.pop_front();
        }
        inner.results.push_back(decoded);
    }

    pub(crate) fn take(&self) -> Option<Result<Vec<u8>, WeldError>> {
        self.inner.lock().unwrap().results.pop_front()
    }
}

fn decode_png(result: Option<&[u8]>) -> Result<Vec<u8>, WeldError> {
    let result = result.ok_or_else(|| {
        WeldError::BrowserOp("Page.captureScreenshot succeeded without a result payload".into())
    })?;
    let value: serde_json::Value = serde_json::from_slice(result).map_err(|error| {
        WeldError::BrowserOp(format!("invalid Page.captureScreenshot result: {error}"))
    })?;
    let encoded = value
        .get("data")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            WeldError::BrowserOp("Page.captureScreenshot result has no PNG data field".into())
        })?;
    let png = STANDARD.decode(encoded).map_err(|error| {
        WeldError::BrowserOp(format!(
            "invalid base64 PNG from Page.captureScreenshot: {error}"
        ))
    })?;
    if !png.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Err(WeldError::BrowserOp(
            "Page.captureScreenshot returned bytes without PNG magic".into(),
        ));
    }
    Ok(png)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captures_decode_only_their_own_result() {
        let snapshots = SnapshotChannel::default();
        snapshots.begin(7).unwrap();
        snapshots.complete(6, true, Some(br#"{"data":"iVBORw0KGgo="}"#));
        assert!(snapshots.take().is_none());
        snapshots.complete(7, true, Some(br#"{"data":"iVBORw0KGgo="}"#));
        assert_eq!(snapshots.take().unwrap().unwrap(), b"\x89PNG\r\n\x1a\n");
    }

    #[test]
    fn invalid_cdp_answers_become_visible_errors() {
        let snapshots = SnapshotChannel::default();
        snapshots.begin(1).unwrap();
        snapshots.complete(1, true, Some(br#"{}"#));
        assert!(matches!(
            snapshots.take(),
            Some(Err(WeldError::BrowserOp(_)))
        ));
    }
}
