// Copyright 2026 Mark Alan Boykin
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// SPDX-License-Identifier: MPL-2.0

//! welding's `CefApp`: the renderer-side half of the runtime.
//!
//! CEF splits work across processes, and a `CefApp` is the only hook that
//! exists in *both*. Without one, a host is confined to what the browser
//! process can answer on its own, which is why script results were previously
//! impossible rather than merely unwired: JavaScript evaluates in the renderer,
//! and nothing was listening there.
//!
//! The app is constructed identically in the browser process (passed to
//! `cef_initialize`) and in every subprocess (passed to `cef_execute_process`),
//! because CEF hands the same object to both roles and takes the handlers each
//! one needs.

use std::sync::{Arc, Mutex};

use crate::{
    error::WeldError,
    surface::{CefSurfaceEvent, WebEventQueue, WebRequestId},
};

/// Accepted script requests that have not settled yet.
#[derive(Debug)]
pub(crate) struct PendingScripts {
    ids: Mutex<Vec<WebRequestId>>,
    events: Arc<WebEventQueue>,
}

impl PendingScripts {
    pub(crate) fn new(events: Arc<WebEventQueue>) -> Self {
        Self {
            ids: Mutex::new(Vec::new()),
            events,
        }
    }

    pub(crate) fn begin(&self, id: WebRequestId) -> Result<(), WeldError> {
        let mut ids = self.ids.lock().unwrap();
        if ids.contains(&id) {
            return Err(WeldError::BrowserOp(
                "that script request id is already in flight".into(),
            ));
        }
        ids.push(id);
        Ok(())
    }

    pub(crate) fn complete(&self, id: WebRequestId, result: Result<String, String>) {
        let accepted = {
            let mut ids = self.ids.lock().unwrap();
            ids.iter()
                .position(|pending| *pending == id)
                .map(|index| ids.remove(index))
                .is_some()
        };
        if accepted {
            self.events
                .push(CefSurfaceEvent::ScriptCompleted { id, result });
        }
    }

    pub(crate) fn fail_all(&self, reason: &str) {
        let ids = std::mem::take(&mut *self.ids.lock().unwrap());
        for id in ids {
            self.events.push(CefSurfaceEvent::ScriptCompleted {
                id,
                result: Err(reason.to_owned()),
            });
        }
    }
}

/// Message names on the browser/renderer channel.
pub(crate) const EVAL_REQUEST: &str = "weld.eval";
pub(crate) const EVAL_RESULT: &str = "weld.eval.result";

/// Wrap the caller's script so the renderer always returns a JSON string.
///
/// The indirection through a function keeps a bare expression working (`2+2`)
/// alongside statements, and `JSON.stringify(undefined)` is itself `undefined`
/// rather than a string, so that case is normalised to JSON `null`.
pub(crate) fn wrap_script(script: &str) -> String {
    format!(
        "(function() {{ var __weld = (function() {{ return ({script}); }})(); \
         return __weld === undefined ? \"null\" : JSON.stringify(__weld); }})()"
    )
}

#[cfg(feature = "cef-runtime")]
mod cef_backed {
    use super::{EVAL_REQUEST, EVAL_RESULT};
    use cef::*;
    use std::sync::Arc;

    cef::wrap_render_process_handler! {
        pub(crate) struct WeldRenderProcessHandler {}

        impl RenderProcessHandler {
            fn on_process_message_received(
                &self,
                _browser: Option<&mut cef::Browser>,
                frame: Option<&mut cef::Frame>,
                _source_process: cef::ProcessId,
                message: Option<&mut cef::ProcessMessage>,
            ) -> ::std::os::raw::c_int {
                let (Some(frame), Some(message)) = (frame, message) else {
                    return 0;
                };
                if cef::CefString::from(&message.name()).to_string() != EVAL_REQUEST {
                    return 0;
                }
                let Some(args) = message.argument_list() else {
                    return 0;
                };
                let id = cef::CefString::from(&args.string(0)).to_string();
                let script = cef::CefString::from(&args.string(1)).to_string();

                // Evaluation has to happen inside the frame's V8 context, and
                // that context only exists here, in the renderer process.
                let (ok, payload) = match frame.v8_context() {
                    Some(ctx) => {
                        let code: cef::CefString = super::wrap_script(&script).as_str().into();
                        let url: cef::CefString = "weld://eval".into();
                        let mut retval: Option<cef::V8Value> = None;
                        let mut exception: Option<cef::V8Exception> = None;
                        let ran = ctx.eval(
                            Some(&code),
                            Some(&url),
                            0,
                            Some(&mut retval),
                            Some(&mut exception),
                        );
                        if ran != 0 {
                            let value = retval
                                .as_ref()
                                .filter(|v| v.is_string() != 0)
                                .map(|v| cef::CefString::from(&v.string_value()).to_string())
                                .unwrap_or_else(|| "null".to_owned());
                            (1, value)
                        } else {
                            let message = exception
                                .as_ref()
                                .map(|e| cef::CefString::from(&e.message()).to_string())
                                .unwrap_or_else(|| "script evaluation failed".to_owned());
                            (0, message)
                        }
                    }
                    None => (0, "frame has no V8 context".to_owned()),
                };

                let name: cef::CefString = EVAL_RESULT.into();
                if let Some(mut reply) = cef::process_message_create(Some(&name)) {
                    if let Some(list) = reply.argument_list() {
                        let id: cef::CefString = id.as_str().into();
                        list.set_string(0, Some(&id));
                        list.set_int(1, ok);
                        let payload: cef::CefString = payload.as_str().into();
                        list.set_string(2, Some(&payload));
                    }
                    frame.send_process_message(cef::ProcessId::BROWSER, Some(&mut reply));
                }
                1
            }
        }
    }

    cef::wrap_app! {
        pub(crate) struct WeldApp {
            switches: Arc<Vec<(String, Option<String>)>>,
            render_process_handler: cef::RenderProcessHandler,
        }

        impl App {
            fn render_process_handler(&self) -> Option<cef::RenderProcessHandler> {
                Some(self.render_process_handler.clone())
            }

            fn on_before_command_line_processing(
                &self,
                _process_type: Option<&cef::CefString>,
                command_line: Option<&mut cef::CommandLine>,
            ) {
                use cef::ImplCommandLine;
                let Some(cmd) = command_line else {
                    return;
                };
                for (name, value) in self.switches.iter() {
                    let name: cef::CefString = name.as_str().into();
                    match value {
                        Some(value) => {
                            let value: cef::CefString = value.as_str().into();
                            cmd.append_switch_with_value(Some(&name), Some(&value));
                        }
                        None => cmd.append_switch(Some(&name)),
                    }
                }
            }
        }
    }

    impl WeldApp {
        pub(crate) fn build(switches: Arc<Vec<(String, Option<String>)>>) -> cef::App {
            Self::new(switches, WeldRenderProcessHandler::new())
        }
    }
}

#[cfg(feature = "cef-runtime")]
pub(crate) use cef_backed::WeldApp;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bare_expression_survives_wrapping() {
        assert!(wrap_script("2+2").contains("return (2+2)"));
    }

    #[test]
    fn undefined_is_normalised_to_json_null() {
        // JSON.stringify(undefined) is undefined, not a string, so without this
        // the renderer would have nothing to send back.
        assert!(wrap_script("whatever").contains("__weld === undefined"));
    }

    #[test]
    fn accepted_scripts_settle_once_and_duplicate_ids_are_refused() {
        let events = Arc::new(WebEventQueue::default());
        let scripts = PendingScripts::new(events.clone());
        let id = WebRequestId::new(u64::MAX);
        scripts.begin(id).unwrap();
        assert!(scripts.begin(id).is_err());
        scripts.complete(id, Ok("4".into()));
        scripts.complete(id, Ok("duplicate".into()));
        assert!(matches!(
            events.poll(),
            Some(CefSurfaceEvent::ScriptCompleted { id: seen, result: Ok(value) })
                if seen == id && value == "4"
        ));
        assert!(events.poll().is_none());
    }

    #[test]
    fn renderer_loss_fails_pending_scripts_in_acceptance_order() {
        let events = Arc::new(WebEventQueue::default());
        let scripts = PendingScripts::new(events.clone());
        scripts.begin(WebRequestId::new(8)).unwrap();
        scripts.begin(WebRequestId::new(3)).unwrap();
        scripts.fail_all("renderer exited");
        for expected in [8, 3] {
            assert!(matches!(
                events.poll(),
                Some(CefSurfaceEvent::ScriptCompleted { id, result: Err(reason) })
                    if id.get() == expected && reason == "renderer exited"
            ));
        }
        assert!(events.poll().is_none());
    }
}
