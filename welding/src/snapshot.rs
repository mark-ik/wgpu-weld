// Copyright 2026 Mark Alan Boykin
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// SPDX-License-Identifier: MPL-2.0

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

use crate::{SnapshotPngCompletion, SnapshotRequestId, WeldError};

const MAX_ADMITTED_CAPTURES: usize = 16;

#[derive(Default)]
pub(crate) struct SnapshotChannel {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    waiting: HashSet<SnapshotRequestId>,
    results: VecDeque<SnapshotPngCompletion>,
}

impl SnapshotChannel {
    pub(crate) fn begin(&self, id: SnapshotRequestId) -> Result<(), WeldError> {
        let mut inner = self.inner.lock().unwrap();
        if !inner.waiting.insert(id) {
            return Err(WeldError::BrowserOp(format!(
                "snapshot request id {id} is already pending"
            )));
        }
        if inner.waiting.len() + inner.results.len() > MAX_ADMITTED_CAPTURES {
            inner.waiting.remove(&id);
            return Err(WeldError::BrowserOp(format!(
                "PNG snapshot backlog is full ({MAX_ADMITTED_CAPTURES} admitted captures); poll completed captures before requesting another"
            )));
        }
        Ok(())
    }

    pub(crate) fn complete(&self, id: i32, success: bool, result: Option<&[u8]>) {
        let id = SnapshotRequestId::from_cef_message_id(id);
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

        // Admission accounts for both pending requests and completed results,
        // so every completion here has a retained slot until a host polls it.
        inner.results.push_back(SnapshotPngCompletion {
            id,
            result: decoded,
        });
    }

    pub(crate) fn take(&self) -> Option<SnapshotPngCompletion> {
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
        let request = SnapshotRequestId::from_cef_message_id(7);
        snapshots.begin(request).unwrap();
        snapshots.complete(6, true, Some(br#"{"data":"iVBORw0KGgo="}"#));
        assert!(snapshots.take().is_none());
        snapshots.complete(7, true, Some(br#"{"data":"iVBORw0KGgo="}"#));
        let completion = snapshots.take().unwrap();
        assert_eq!(completion.id, request);
        assert_eq!(completion.result.unwrap(), b"\x89PNG\r\n\x1a\n");
    }

    #[test]
    fn invalid_cdp_answers_become_visible_errors() {
        let snapshots = SnapshotChannel::default();
        snapshots
            .begin(SnapshotRequestId::from_cef_message_id(1))
            .unwrap();
        snapshots.complete(1, true, Some(br#"{}"#));
        assert!(matches!(
            snapshots.take().map(|completion| completion.result),
            Some(Err(WeldError::BrowserOp(_)))
        ));
    }

    #[test]
    fn admitted_results_are_never_evicted() {
        let snapshots = SnapshotChannel::default();
        for id in 1..=MAX_ADMITTED_CAPTURES as i32 {
            snapshots
                .begin(SnapshotRequestId::from_cef_message_id(id))
                .unwrap();
        }
        assert!(matches!(
            snapshots.begin(SnapshotRequestId::from_cef_message_id(17)),
            Err(WeldError::BrowserOp(message)) if message.contains("backlog is full")
        ));

        for id in 1..=MAX_ADMITTED_CAPTURES as i32 {
            snapshots.complete(id, true, Some(br#"{"data":"iVBORw0KGgo="}"#));
        }
        for id in 1..=MAX_ADMITTED_CAPTURES as i32 {
            let completion = snapshots.take().unwrap();
            assert_eq!(completion.id, SnapshotRequestId::from_cef_message_id(id));
            assert!(completion.result.is_ok());
        }
        assert!(snapshots.take().is_none());
    }
}
