// Copyright 2026 Mark Alan Boykin
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// SPDX-License-Identifier: MPL-2.0

//! Translation between welding's portable drag/touch types and CEF.

use cef::{ImplBrowserHost, ImplDragData};

use crate::{
    WeldError,
    surface::{
        ContactDevice, DragEventKind, DragFile, DragInput, DragOperations, DragPayload,
        EventModifiers, TouchInput, TouchPhase,
    },
};

const EVENTFLAG_SHIFT_DOWN: u32 = 1 << 1;
const EVENTFLAG_CONTROL_DOWN: u32 = 1 << 2;
const EVENTFLAG_ALT_DOWN: u32 = 1 << 3;
const EVENTFLAG_LEFT_MOUSE_BUTTON: u32 = 1 << 4;
const EVENTFLAG_MIDDLE_MOUSE_BUTTON: u32 = 1 << 5;
const EVENTFLAG_RIGHT_MOUSE_BUTTON: u32 = 1 << 6;
const EVENTFLAG_COMMAND_DOWN: u32 = 1 << 7;

fn modifiers(modifiers: EventModifiers) -> u32 {
    let mut flags = 0;
    if modifiers.shift {
        flags |= EVENTFLAG_SHIFT_DOWN;
    }
    if modifiers.ctrl {
        flags |= EVENTFLAG_CONTROL_DOWN;
    }
    if modifiers.alt {
        flags |= EVENTFLAG_ALT_DOWN;
    }
    if modifiers.left_mouse_button {
        flags |= EVENTFLAG_LEFT_MOUSE_BUTTON;
    }
    if modifiers.middle_mouse_button {
        flags |= EVENTFLAG_MIDDLE_MOUSE_BUTTON;
    }
    if modifiers.right_mouse_button {
        flags |= EVENTFLAG_RIGHT_MOUSE_BUTTON;
    }
    if modifiers.meta {
        flags |= EVENTFLAG_COMMAND_DOWN;
    }
    flags
}

fn operations(operations: DragOperations) -> cef::DragOperationsMask {
    cef::DragOperationsMask::from(cef::sys::cef_drag_operations_mask_t(operations.0 as _))
}

pub(crate) fn send_touch(host: &cef::BrowserHost, event: TouchInput, scale: f32) {
    let phase = match event.phase {
        TouchPhase::Started => cef::TouchEventType::PRESSED,
        TouchPhase::Moved => cef::TouchEventType::MOVED,
        TouchPhase::Ended => cef::TouchEventType::RELEASED,
        TouchPhase::Cancelled => cef::TouchEventType::CANCELLED,
    };
    let event = cef::TouchEvent {
        id: event.id,
        x: event.x / scale,
        y: event.y / scale,
        radius_x: event.radius_x / scale,
        radius_y: event.radius_y / scale,
        rotation_angle: event.rotation_angle,
        pressure: event.pressure.clamp(0.0, 1.0),
        type_: phase,
        modifiers: modifiers(event.modifiers),
        pointer_type: match event.device {
            ContactDevice::Touch => cef::PointerType::TOUCH,
            ContactDevice::Pen => cef::PointerType::PEN,
        },
    };
    host.send_touch_event(Some(&event));
}

pub(crate) fn send_drag(
    host: &cef::BrowserHost,
    event: DragInput,
    scale: f32,
) -> Result<(), WeldError> {
    let mouse = cef::MouseEvent {
        x: (event.x as f32 / scale).round() as i32,
        y: (event.y as f32 / scale).round() as i32,
        modifiers: modifiers(event.modifiers),
    };
    let allowed = operations(event.allowed_operations);

    match event.kind {
        DragEventKind::Enter => {
            let payload = event.payload.ok_or_else(|| {
                WeldError::BrowserOp("DragInput::Enter requires a DragPayload".into())
            })?;
            let mut data = to_cef_payload(&payload)?;
            host.drag_target_drag_enter(Some(&mut data), Some(&mouse), allowed);
        }
        DragEventKind::Over => host.drag_target_drag_over(Some(&mouse), allowed),
        DragEventKind::Leave => host.drag_target_drag_leave(),
        DragEventKind::Drop => host.drag_target_drop(Some(&mouse)),
    }
    Ok(())
}

pub(crate) fn finish_drag_source(
    host: &cef::BrowserHost,
    x: i32,
    y: i32,
    operation: DragOperations,
    scale: f32,
) {
    host.drag_source_ended_at(
        (x as f32 / scale).round() as i32,
        (y as f32 / scale).round() as i32,
        operations(operation),
    );
    host.drag_source_system_drag_ended();
}

/// Copy callback-scoped CEF drag data into welding-owned values before handing
/// it to the host. Image data is intentionally not exposed yet: CEF offers it
/// as a platform `CefImage`, while all of file/link/text/html survive a normal
/// cross-toolkit OS drag unchanged.
pub(crate) fn payload_from_cef(data: &cef::DragData) -> DragPayload {
    let mut payload = DragPayload::default();

    if data.is_file() != 0
        && let Some(mut paths) = cef::string_list_alloc()
    {
        data.file_paths(Some(&mut paths));
        let count = cef::string_list_size(Some(&mut paths));
        for index in 0..count {
            let mut path = cef::CefString::default();
            if cef::string_list_value(Some(&mut paths), index, Some(&mut path)) != 0 {
                payload.files.push(DragFile {
                    path: std::path::PathBuf::from(path.to_string()),
                    display_name: None,
                });
            }
        }
    }
    if data.is_link() != 0 {
        payload.link_url = non_empty(userfree(data.link_url()));
        payload.link_title = non_empty(userfree(data.link_title()));
    }
    if data.is_fragment() != 0 {
        payload.fragment_text = non_empty(userfree(data.fragment_text()));
        payload.fragment_html = non_empty(userfree(data.fragment_html()));
        payload.fragment_base_url = non_empty(userfree(data.fragment_base_url()));
    }
    payload
}

fn to_cef_payload(payload: &DragPayload) -> Result<cef::DragData, WeldError> {
    let data = cef::drag_data_create()
        .ok_or_else(|| WeldError::BrowserOp("cef_drag_data_create returned None".into()))?;

    for file in &payload.files {
        if !file.path.is_absolute() {
            return Err(WeldError::BrowserOp(format!(
                "dragged file path must be absolute: {}",
                file.path.display()
            )));
        }
        let path: cef::CefString = file.path.to_string_lossy().as_ref().into();
        let display = file.display_name.clone().unwrap_or_else(|| {
            file.path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| file.path.to_string_lossy().into_owned())
        });
        let display: cef::CefString = display.as_str().into();
        data.add_file(Some(&path), Some(&display));
    }
    if let Some(value) = payload.link_url.as_deref() {
        let value: cef::CefString = value.into();
        data.set_link_url(Some(&value));
    }
    if let Some(value) = payload.link_title.as_deref() {
        let value: cef::CefString = value.into();
        data.set_link_title(Some(&value));
    }
    if let Some(value) = payload.fragment_text.as_deref() {
        let value: cef::CefString = value.into();
        data.set_fragment_text(Some(&value));
    }
    if let Some(value) = payload.fragment_html.as_deref() {
        let value: cef::CefString = value.into();
        data.set_fragment_html(Some(&value));
    }
    if let Some(value) = payload.fragment_base_url.as_deref() {
        let value: cef::CefString = value.into();
        data.set_fragment_base_url(Some(&value));
    }
    Ok(data)
}

fn userfree(value: cef::CefStringUserfree) -> String {
    let raw: Option<&cef::sys::_cef_string_utf16_t> = (&value).into();
    raw.map(|raw| cef::CefStringUtf16::from(*raw).to_string())
        .unwrap_or_default()
}

fn non_empty(value: String) -> Option<String> {
    (!value.is_empty()).then_some(value)
}
