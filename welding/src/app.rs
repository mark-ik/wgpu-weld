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

use std::sync::Mutex;

/// A finished `request_script_result`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScriptResult {
    pub id: u32,
    /// `Ok` carries the value as JSON; `Err` carries the exception message.
    ///
    /// JSON rather than a bare string because a script can return anything and
    /// the alternatives lose information: `document.title` arrives as
    /// `"Example Domain"` quoted, `2+2` as `4`, an object as an object.
    pub value: Result<String, String>,
}

/// Results returned from the renderer, waiting to be polled.
#[derive(Debug, Default)]
pub(crate) struct ScriptResults(Mutex<Vec<ScriptResult>>);

impl ScriptResults {
    pub(crate) fn push(&self, result: ScriptResult) {
        self.0.lock().unwrap().push(result);
    }

    pub(crate) fn take_one(&self) -> Option<ScriptResult> {
        let mut queue = self.0.lock().unwrap();
        if queue.is_empty() {
            None
        } else {
            Some(queue.remove(0))
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
                let id = args.int(0);
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
                        list.set_int(0, id);
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
    fn results_are_delivered_once_each_in_order() {
        let results = ScriptResults::default();
        assert_eq!(results.take_one(), None);
        results.push(ScriptResult {
            id: 1,
            value: Ok("4".into()),
        });
        results.push(ScriptResult {
            id: 2,
            value: Err("boom".into()),
        });
        assert_eq!(results.take_one().unwrap().id, 1);
        assert_eq!(results.take_one().unwrap().id, 2);
        assert_eq!(results.take_one(), None);
    }

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
}
