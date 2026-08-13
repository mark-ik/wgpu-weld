//! Unattended input: the gestures a machine nobody is sitting at cannot make.
//!
//! Chromium reveals parts of itself only in response to real input. A link
//! does not navigate, a page does not scroll, and a key never reaches the DOM
//! without one, so a headless parity run that skips input proves nothing about
//! the input path. These drive the producer directly.
//!
//! - `WELD_CLICK_AT=x,y` clicks once, in physical pixels.
//! - `WELD_WHEEL=dy` scrolls by `dy` after the click.
//! - `WELD_KEY=c` types one character after that.
//! - `WELD_CRASH_AFTER_SECS=n` navigates to `chrome://crash` to kill the
//!   render process on purpose, so crash recovery can be exercised.
//! - `WELD_RECOVER=1` recovers from that crash; see `recover_if_crashed`.
//! - `WELD_IME=text` composes `text` through the IME path, then commits it.
//! - `WELD_HIDE_CYCLE=1` hides the browser, then shows it again, which is how
//!   `set_visible` gets checked: painting should stop while hidden.
//! - `WELD_DEVTOOLS=1` opens the DevTools window.
//!
//! Point them at a page that reports what it received (writing to
//! `document.title` surfaces in the navigation events) and each one becomes a
//! checkable claim rather than a hope.

use std::time::{Duration, Instant};

use welding::{
    CefSurfaceProducer, EventModifiers, FocusDirection, KeyEvent, KeyEventKind, MouseAction,
    MouseButton, MouseEvent, NavigationEvent, ProcessTerminationStatus,
};

/// Wait after the page is ready before acting: the first paint can land before
/// the page's own scripts and layout have settled.
const SETTLE: Duration = Duration::from_secs(3);

/// Time between one gesture and the next, so their effects stay attributable.
const GAP: Duration = Duration::from_secs(2);

pub struct ScriptedInput {
    click_at: Option<(i32, i32)>,
    wheel: Option<i32>,
    key: Option<char>,
    crash_after: Option<Duration>,
    ime: Option<String>,
    hide_cycle: bool,
    devtools: bool,
    /// When the page first became ready. Elapsed time, not a tick count: how
    /// often a host redraws is its own business, and an accelerated producer
    /// paints only on change, so ticks are not a clock.
    ready_at: Option<Instant>,
    stage: u32,
    crashed: bool,
}

impl ScriptedInput {
    pub fn from_env() -> Self {
        let click_at = std::env::var("WELD_CLICK_AT").ok().and_then(|v| {
            let (x, y) = v.split_once(',')?;
            Some((x.trim().parse().ok()?, y.trim().parse().ok()?))
        });
        let wheel = std::env::var("WELD_WHEEL")
            .ok()
            .and_then(|v| v.trim().parse::<i32>().ok());
        let key = std::env::var("WELD_KEY")
            .ok()
            .and_then(|v| v.chars().next());
        let crash_after = std::env::var("WELD_CRASH_AFTER_SECS")
            .ok()
            .and_then(|v| v.trim().parse::<u64>().ok())
            .map(Duration::from_secs);
        let ime = std::env::var("WELD_IME").ok().filter(|v| !v.is_empty());
        let hide_cycle = std::env::var("WELD_HIDE_CYCLE").is_ok();
        let devtools = std::env::var("WELD_DEVTOOLS").is_ok();
        Self {
            click_at,
            wheel,
            key,
            crash_after,
            ime,
            hide_cycle,
            devtools,
            ready_at: None,
            stage: 0,
            crashed: false,
        }
    }

    /// True when anything is scripted at all.
    pub fn armed(&self) -> bool {
        self.click_at.is_some()
            || self.wheel.is_some()
            || self.key.is_some()
            || self.crash_after.is_some()
            || self.ime.is_some()
            || self.hide_cycle
            || self.devtools
    }

    /// Call once per tick, with `ready` set once the page has painted. Fires at
    /// most one gesture per call, in order.
    pub fn tick<P: CefSurfaceProducer + ?Sized>(&mut self, producer: &mut P, ready: bool) {
        if !ready || !self.armed() {
            return;
        }
        let started = *self.ready_at.get_or_insert_with(Instant::now);
        // The crash keeps its own schedule, measured from ready rather than
        // queued behind the gestures, so it can be asked for on its own.
        if let Some(after) = self.crash_after {
            if started.elapsed() >= after && !self.crashed {
                self.crashed = true;
                eprintln!("weld demo: navigating to chrome://crash on purpose");
                let _ = producer.navigate_to_url("chrome://crash");
            }
        }
        if started.elapsed() < SETTLE + GAP * self.stage {
            return;
        }
        match self.stage {
            0 => self.click(producer),
            1 => self.scroll(producer),
            2 => self.press(producer),
            3 => self.compose(producer),
            4 => self.hide(producer),
            5 => self.show(producer),
            6 => self.devtools(producer),
            _ => {}
        }
        self.stage += 1;
    }

    fn click<P: CefSurfaceProducer + ?Sized>(&self, producer: &mut P) {
        let Some((x, y)) = self.click_at else { return };
        eprintln!("weld demo: scripted click at {x},{y} (scale {})", producer.scale_factor());
        // A window launched without activation never sees Focused(true), and
        // CEF routes hover, cursor and key updates only to a focused browser.
        let _ = producer.move_focus(FocusDirection::Forward);
        for action in [MouseAction::Moved, MouseAction::Pressed, MouseAction::Released] {
            let _ = producer.send_mouse_input(MouseEvent {
                x,
                y,
                button: MouseButton::Left,
                action,
                modifiers: EventModifiers::default(),
            });
        }
    }

    fn scroll<P: CefSurfaceProducer + ?Sized>(&self, producer: &mut P) {
        let Some(delta_y) = self.wheel else { return };
        let (x, y) = self.click_at.unwrap_or((10, 10));
        eprintln!("weld demo: scripted wheel dy={delta_y}");
        let _ = producer.send_mouse_input(MouseEvent {
            x,
            y,
            button: MouseButton::Left,
            action: MouseAction::WheelScrolled { delta_x: 0, delta_y },
            modifiers: EventModifiers::default(),
        });
    }

    fn press<P: CefSurfaceProducer + ?Sized>(&self, producer: &mut P) {
        let Some(ch) = self.key else { return };
        eprintln!("weld demo: scripted key {ch:?}");
        // Chromium wants a Windows virtual-key code on every platform. For an
        // ASCII letter that is the uppercase codepoint.
        let vk = ch.to_ascii_uppercase() as i32;
        for kind in [KeyEventKind::RawKeyDown, KeyEventKind::Char, KeyEventKind::KeyUp] {
            let _ = producer.send_keyboard_input(KeyEvent {
                kind,
                windows_key_code: vk,
                native_key_code: 0,
                character: Some(ch),
                modifiers: EventModifiers::default(),
            });
        }
    }

    fn compose<P: CefSurfaceProducer + ?Sized>(&self, producer: &mut P) {
        let Some(text) = self.ime.as_deref() else { return };
        // WELD_IME_MODE picks which half of the IME path runs, so a failure can
        // be pinned to the composition or to the commit rather than to "IME".
        let mode = std::env::var("WELD_IME_MODE").unwrap_or_else(|_| "both".into());
        eprintln!("weld demo: IME composing {text:?} (mode {mode})");
        let end = text.chars().count() as u32;
        if mode != "commit" {
            if let Err(e) = producer.ime_set_composition(text, (end, end)) {
                eprintln!("weld demo: ime_set_composition failed: {e}");
                return;
            }
        }
        match mode.as_str() {
            "compose" => {}
            "finish" => {
                if let Err(e) = producer.ime_finish_composing(false) {
                    eprintln!("weld demo: ime_finish_composing failed: {e}");
                }
            }
            _ => {
                if let Err(e) = producer.ime_commit_text(text) {
                    eprintln!("weld demo: ime_commit_text failed: {e}");
                }
            }
        }
    }

    fn hide<P: CefSurfaceProducer + ?Sized>(&self, producer: &mut P) {
        if !self.hide_cycle {
            return;
        }
        eprintln!("weld demo: set_visible(false) -- painting should stop here");
        if let Err(e) = producer.set_visible(false) {
            eprintln!("weld demo: set_visible(false) failed: {e}");
        }
    }

    fn show<P: CefSurfaceProducer + ?Sized>(&self, producer: &mut P) {
        if !self.hide_cycle {
            return;
        }
        eprintln!("weld demo: set_visible(true) -- painting should resume here");
        if let Err(e) = producer.set_visible(true) {
            eprintln!("weld demo: set_visible(true) failed: {e}");
        }
    }

    fn devtools<P: CefSurfaceProducer + ?Sized>(&self, producer: &mut P) {
        if !self.devtools {
            return;
        }
        match producer.open_devtools() {
            Ok(()) => eprintln!("weld demo: open_devtools() ok"),
            Err(e) => eprintln!("weld demo: open_devtools() failed: {e}"),
        }
    }
}

/// `WELD_AUTH=user:pass` answers an auth challenge as it arrives.
///
/// Credentials come from the environment only because a machine nobody is
/// sitting at cannot be asked; they are passed straight to the producer and
/// never logged.
pub fn answer_auth_if_challenged<P: CefSurfaceProducer + ?Sized>(
    producer: &mut P,
    event: &NavigationEvent,
) {
    let NavigationEvent::AuthChallenged { id, host, realm, scheme, is_proxy, .. } = event else {
        return;
    };
    eprintln!(
        "weld demo: auth challenge #{id} host={host} realm={realm:?} scheme={scheme} proxy={is_proxy}"
    );
    let Ok(spec) = std::env::var("WELD_AUTH") else { return };
    let (user, pass) = spec.split_once(':').unwrap_or((spec.as_str(), ""));
    match producer.answer_auth(*id, user, pass) {
        Ok(()) => eprintln!("weld demo: answered auth #{id}"),
        Err(e) => eprintln!("weld demo: answer_auth failed: {e}"),
    }
}

/// `WELD_RECOVER` sends the browser back to its starting page when the render
/// process dies, which is the whole crash-recovery story in one place.
///
/// Back to the original URL rather than a reload, because the page that killed
/// the renderer would just kill its replacement. And navigating is not enough
/// on its own: painting is change-driven and the fresh renderer has nothing
/// queued for this surface, so without `request_repaint` the host would
/// present its pre-crash frame forever.
pub fn recover_if_crashed<P: CefSurfaceProducer + ?Sized>(
    producer: &mut P,
    url: &str,
    event: &NavigationEvent,
) {
    let NavigationEvent::ContentProcessTerminated { status, .. } = event else {
        return;
    };
    if std::env::var("WELD_RECOVER").is_err() {
        return;
    }
    // Not for OutOfMemory: retrying that only reaches it again, slower.
    if *status == ProcessTerminationStatus::OutOfMemory {
        eprintln!("weld demo: renderer died out of memory; not retrying");
        return;
    }
    eprintln!("weld demo: recovering from {status:?}");
    if let Err(e) = producer.navigate_to_url(url) {
        eprintln!("weld demo: recovery navigation failed: {e}");
    }
    if let Err(e) = producer.request_repaint() {
        eprintln!("weld demo: repaint request failed: {e}");
    }
}
