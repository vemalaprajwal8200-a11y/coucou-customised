// Island window: placement on the chosen display, the two window sizes
// (full panel / invisible wake strip), click-through and the cursor poll.
//
// There is no notch on a PC, so the island is a black shape drawn at the top
// centre of the main display inside a borderless, transparent, always-on-top
// window that never takes focus.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, Monitor, PhysicalPosition, PhysicalSize, WebviewWindow};

use crate::platform::{self, cursor_physical, left_button_down};

/// Logical size of the full window — the largest island view, like the macOS panel.
pub const PANEL_W: f64 = 720.0;
pub const PANEL_H: f64 = 320.0;
/// Logical size of the invisible strip that wakes the island when it is hidden.
pub const STRIP_W: f64 = 240.0;
pub const STRIP_H: f64 = 6.0;

pub const AUTO_HIDE_DELAY_MS: u64 = 5000;
pub const LAUNCH_VISIBLE_MS: u64 = 10_000;
pub const TRIGGER_ZONE_HEIGHT_PX: i32 = 2;
pub const TRIGGER_MARGIN_PX: i32 = 100;
pub const ANIMATION_MS: u64 = 220;
pub const POLL_INTERVAL_MS: u64 = 60;
pub const FULL_SCREEN_TRIGGER_ZONE: bool = false;
const ANIMATION_FRAME_MS: u64 = 16;
const HIDDEN_EDGE_GAP_PX: i32 = 2;

pub const WINDOW_LABEL: &str = "island";

/// Margin around the island that still counts as "on the island", in logical px.
/// Wider than the macOS 6 pt because a click must never be swallowed.
const HIT_MARGIN: f64 = 14.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AutoHidePhase {
    Hidden,
    Revealing,
    Visible,
    Hiding,
}

struct AutoHideController {
    enabled: bool,
    phase: AutoHidePhase,
    animation_started: Option<Instant>,
    animation_from_y: i32,
    hide_deadline: Option<Instant>,
    hovering: bool,
    reveal_requested: bool,
}

impl AutoHideController {
    fn new(enabled: bool) -> Self {
        Self {
            enabled,
            phase: if enabled {
                AutoHidePhase::Hidden
            } else {
                AutoHidePhase::Visible
            },
            animation_started: None,
            animation_from_y: 0,
            hide_deadline: None,
            hovering: false,
            reveal_requested: false,
        }
    }
}

#[derive(Serialize, Clone)]
pub struct CursorPayload {
    pub x: f64,
    pub y: f64,
}

#[derive(Serialize, Clone)]
pub struct ScreenInfo {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub scale: f64,
}

/// The island shape in window-logical coordinates, pushed by the front end.
/// The poll thread owns the click-through decision so it lands in the same 16 ms
/// tick as the cursor read — an IPC round trip here loses clicks.
#[derive(Clone, Copy, Default)]
pub struct IslandRect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// Wakes / parks the cursor poll thread so a hidden island costs literally nothing.
pub struct PollGate {
    active: Mutex<bool>,
    cv: Condvar,
    pub collapsed: AtomicBool,
    voice_active: AtomicBool,
    wake_conversation_active: AtomicBool,
    pub rect: Mutex<IslandRect>,
    /// Mirrors the window flag so we only call into the OS when it changes.
    ignoring: AtomicBool,
    auto_hide: Mutex<AutoHideController>,
}

impl PollGate {
    pub fn new(auto_hide: bool) -> Self {
        Self {
            active: Mutex::new(false),
            cv: Condvar::new(),
            collapsed: AtomicBool::new(true),
            voice_active: AtomicBool::new(false),
            wake_conversation_active: AtomicBool::new(false),
            rect: Mutex::new(IslandRect::default()),
            ignoring: AtomicBool::new(false),
            auto_hide: Mutex::new(AutoHideController::new(auto_hide)),
        }
    }

    pub fn auto_hide_enabled(&self) -> bool {
        platform::CURSOR_POLL && self.auto_hide.lock().unwrap().enabled
    }

    fn auto_hide_phase(&self) -> AutoHidePhase {
        self.auto_hide.lock().unwrap().phase
    }

    fn should_poll(&self) -> bool {
        self.is_active() || self.auto_hide_enabled()
    }

    pub fn set_rect(&self, rect: IslandRect) {
        *self.rect.lock().unwrap() = rect;
    }

    /// Forces the next poll tick to re-apply the flag (after a window resize).
    pub fn forget_ignore_state(&self) {
        self.ignoring.store(false, Ordering::Relaxed);
    }

    pub fn set_active(&self, on: bool) {
        let mut guard = self.active.lock().unwrap();
        *guard = on;
        self.cv.notify_all();
    }

    fn wait_until_active(&self) {
        let mut guard = self.active.lock().unwrap();
        while !*guard {
            guard = self.cv.wait(guard).unwrap();
        }
    }

    fn is_active(&self) -> bool {
        *self.active.lock().unwrap()
    }
}

/// Start at the visible position, then let normal auto-hide resume after the
/// launch grace period. Other auto-hide interactions remain unchanged.
pub fn prepare_launch_visibility(gate: &PollGate) {
    let now = Instant::now();
    let mut controller = gate.auto_hide.lock().unwrap();
    controller.phase = AutoHidePhase::Visible;
    controller.animation_started = None;
    controller.hide_deadline = controller
        .enabled
        .then(|| now + Duration::from_millis(LAUNCH_VISIBLE_MS));
    controller.hovering = false;
    controller.reveal_requested = false;
}

pub fn window(app: &AppHandle) -> Option<WebviewWindow> {
    app.get_webview_window(WINDOW_LABEL)
}

fn monitor_contains(m: &Monitor, x: f64, y: f64) -> bool {
    let p = m.position();
    let s = m.size();
    x >= p.x as f64
        && x < (p.x + s.width as i32) as f64
        && y >= p.y as f64
        && y < (p.y + s.height as i32) as f64
}

/// The display the island lives on: the primary one, or the one under the cursor.
fn target_monitor(app: &AppHandle, pref: &str) -> Option<Monitor> {
    let monitors = app.available_monitors().ok()?;
    if pref == "cursor" {
        if let Some((cx, cy)) = cursor_physical() {
            if let Some(m) = monitors.iter().find(|m| monitor_contains(m, cx, cy)) {
                return Some(m.clone());
            }
        }
    }
    app.primary_monitor()
        .ok()
        .flatten()
        .or_else(|| monitors.into_iter().next())
}

pub fn screen_info(app: &AppHandle, pref: &str) -> ScreenInfo {
    match target_monitor(app, pref) {
        Some(m) => {
            let scale = m.scale_factor();
            let p = m.position();
            let s = m.size();
            ScreenInfo {
                x: p.x as f64 / scale,
                y: p.y as f64 / scale,
                width: s.width as f64 / scale,
                height: s.height as f64 / scale,
                scale,
            }
        }
        None => ScreenInfo {
            x: 0.0,
            y: 0.0,
            width: 1920.0,
            height: 1080.0,
            scale: 1.0,
        },
    }
}

/// Places and sizes the window. `collapsed` picks the wake strip instead of the panel.
pub fn apply_geometry(app: &AppHandle, pref: &str, collapsed: bool) {
    let Some(win) = window(app) else { return };
    let Some(m) = target_monitor(app, pref) else {
        return;
    };

    let scale = m.scale_factor();
    let mp = *m.position();
    let ms = *m.size();

    let auto_hide = app
        .try_state::<crate::Shared>()
        .map(|shared| shared.gate.auto_hide_enabled())
        .unwrap_or(false);
    let (lw, lh) = if collapsed && !auto_hide {
        (STRIP_W, STRIP_H)
    } else {
        (PANEL_W, PANEL_H)
    };
    let pw = (lw * scale).round().max(1.0) as u32;
    let ph = (lh * scale).round().max(1.0) as u32;
    let x = mp.x + (ms.width as i32 - pw as i32) / 2;
    let current_y = win.outer_position().map(|p| p.y).unwrap_or(mp.y);
    let y = if auto_hide {
        app.try_state::<crate::Shared>()
            .map(|shared| {
                let controller = shared.gate.auto_hide.lock().unwrap();
                match controller.phase {
                    AutoHidePhase::Hidden => mp.y - ph as i32 - HIDDEN_EDGE_GAP_PX,
                    AutoHidePhase::Revealing | AutoHidePhase::Hiding => current_y,
                    AutoHidePhase::Visible => mp.y,
                }
            })
            .unwrap_or(mp.y)
    } else {
        mp.y
    };

    // GTK never sizes a non-resizable window below its natural size (200 px
    // here), so on Linux the 6 px wake strip would stay a 200 px block. tao
    // re-applies the config's `resizable: false` after the first configure, so
    // this is asked every time, just before the resize. Undecorated, the window
    // still offers the user nothing to resize it by. (Found by @YossiYad, #44.)
    #[cfg(target_os = "linux")]
    let _ = win.set_resizable(true);
    let _ = win.set_size(PhysicalSize::new(pw, ph));
    let _ = win.set_position(PhysicalPosition::new(x, y));
    // Moving across displays can rescale the window: re-assert the physical size.
    let _ = win.set_size(PhysicalSize::new(pw, ph));
    let _ = win.set_always_on_top(true);
}

pub fn set_auto_hide_enabled(app: &AppHandle, gate: &PollGate, enabled: bool) {
    {
        let mut controller = gate.auto_hide.lock().unwrap();
        controller.enabled = enabled;
        controller.phase = AutoHidePhase::Visible;
        controller.animation_started = None;
        controller.hide_deadline =
            enabled.then(|| Instant::now() + Duration::from_millis(AUTO_HIDE_DELAY_MS));
        controller.hovering = false;
        controller.reveal_requested = false;
    }

    gate.set_active(!gate.collapsed.load(Ordering::Relaxed) || gate.auto_hide_enabled());
    let pref = app
        .try_state::<crate::Shared>()
        .map(|shared| shared.settings.lock().unwrap().screen.clone())
        .unwrap_or_else(|| "primary".into());
    apply_geometry(app, &pref, gate.collapsed.load(Ordering::Relaxed));
    refresh_click_through(app, gate);
}

fn begin_transition(controller: &mut AutoHideController, phase: AutoHidePhase, y: i32) {
    controller.phase = phase;
    controller.animation_started = Some(Instant::now());
    controller.animation_from_y = y;
    controller.hide_deadline = None;
}

pub fn request_auto_reveal(app: &AppHandle, gate: &PollGate) {
    if !gate.auto_hide_enabled() {
        return;
    }
    let Some(win) = window(app) else { return };
    let current_y = win.outer_position().map(|p| p.y).unwrap_or(0);
    let mut controller = gate.auto_hide.lock().unwrap();
    controller.reveal_requested = true;
    if controller.phase == AutoHidePhase::Visible {
        controller.hide_deadline = Some(Instant::now() + Duration::from_millis(AUTO_HIDE_DELAY_MS));
    }
    if !platform::fullscreen_app_active(current_display_bounds(app))
        && matches!(
            controller.phase,
            AutoHidePhase::Hidden | AutoHidePhase::Hiding
        )
    {
        controller.reveal_requested = false;
        controller.hovering = false;
        begin_transition(&mut controller, AutoHidePhase::Revealing, current_y);
    }
}

pub fn set_voice_active(app: &AppHandle, gate: &PollGate, active: bool) {
    gate.voice_active.store(active, Ordering::Relaxed);
    if active {
        request_auto_reveal(app, gate);
        return;
    }
    let mut controller = gate.auto_hide.lock().unwrap();
    if controller.phase == AutoHidePhase::Visible && controller.enabled {
        controller.hide_deadline = Some(Instant::now() + Duration::from_millis(AUTO_HIDE_DELAY_MS));
    }
}

pub fn set_wake_conversation_active(app: &AppHandle, gate: &PollGate, active: bool) {
    gate.wake_conversation_active
        .store(active, Ordering::Relaxed);
    if active {
        request_auto_reveal(app, gate);
        return;
    }
    let mut controller = gate.auto_hide.lock().unwrap();
    if controller.phase == AutoHidePhase::Visible && controller.enabled {
        controller.hide_deadline = Some(Instant::now() + Duration::from_millis(AUTO_HIDE_DELAY_MS));
    }
}

fn current_display_bounds(app: &AppHandle) -> (i32, i32, u32, u32) {
    let pref = app
        .try_state::<crate::Shared>()
        .map(|shared| shared.settings.lock().unwrap().screen.clone())
        .unwrap_or_else(|| "primary".into());
    target_monitor(app, &pref)
        .map(|monitor| {
            let position = *monitor.position();
            let size = *monitor.size();
            (position.x, position.y, size.width, size.height)
        })
        .unwrap_or((0, 0, 1920, 1080))
}

fn point_in_rect(x: f64, y: f64, left: i32, top: i32, width: u32, height: u32) -> bool {
    x >= left as f64
        && x < left as f64 + width as f64
        && y >= top as f64
        && y < top as f64 + height as f64
}

/// Advances the Windows-only native window state. The cursor is sampled by the
/// existing poll thread, so reveal/hide transitions remain interruptible.
fn advance_auto_hide(
    app: &AppHandle,
    gate: &PollGate,
    win: &WebviewWindow,
    cursor: (f64, f64),
    now: Instant,
) -> bool {
    if !gate.auto_hide_enabled() {
        return false;
    }
    let pref = app
        .try_state::<crate::Shared>()
        .map(|shared| shared.settings.lock().unwrap().screen.clone())
        .unwrap_or_else(|| "primary".into());
    let Some(monitor) = target_monitor(app, &pref) else {
        return false;
    };
    let monitor_pos = *monitor.position();
    let monitor_size = *monitor.size();
    let scale = monitor.scale_factor();
    let Ok(position) = win.outer_position() else {
        return false;
    };
    let Ok(size) = win.outer_size() else {
        return false;
    };
    let visible_y = monitor_pos.y;
    let hidden_y = visible_y - size.height as i32 - HIDDEN_EDGE_GAP_PX;
    let notch_width = (STRIP_W * scale).round() as i32;
    let trigger_left = if FULL_SCREEN_TRIGGER_ZONE {
        monitor_pos.x
    } else {
        monitor_pos.x + (monitor_size.width as i32 - notch_width) / 2 - TRIGGER_MARGIN_PX
    };
    let trigger_right = if FULL_SCREEN_TRIGGER_ZONE {
        monitor_pos.x + monitor_size.width as i32
    } else {
        monitor_pos.x + (monitor_size.width as i32 + notch_width) / 2 + TRIGGER_MARGIN_PX
    };
    let in_trigger = cursor.0 >= trigger_left as f64
        && cursor.0 <= trigger_right as f64
        && cursor.1 >= monitor_pos.y as f64
        && cursor.1 <= (monitor_pos.y + TRIGGER_ZONE_HEIGHT_PX) as f64;
    let fullscreen = platform::fullscreen_app_active((
        monitor_pos.x,
        monitor_pos.y,
        monitor_size.width,
        monitor_size.height,
    ));

    let mut controller = gate.auto_hide.lock().unwrap();
    if fullscreen {
        if !matches!(
            controller.phase,
            AutoHidePhase::Hidden | AutoHidePhase::Hiding
        ) {
            begin_transition(&mut controller, AutoHidePhase::Hiding, position.y);
        }
    } else {
        if controller.reveal_requested {
            controller.reveal_requested = false;
            if controller.phase == AutoHidePhase::Visible {
                controller.hide_deadline = Some(now + Duration::from_millis(AUTO_HIDE_DELAY_MS));
            } else if matches!(
                controller.phase,
                AutoHidePhase::Hidden | AutoHidePhase::Hiding
            ) {
                controller.hovering = false;
                begin_transition(&mut controller, AutoHidePhase::Revealing, position.y);
            }
        }
        let over_window = point_in_rect(
            cursor.0,
            cursor.1,
            position.x,
            position.y,
            size.width,
            size.height,
        );
        match controller.phase {
            AutoHidePhase::Hidden if in_trigger => {
                controller.hovering = false;
                begin_transition(&mut controller, AutoHidePhase::Revealing, position.y);
            }
            AutoHidePhase::Hiding if in_trigger || over_window => {
                controller.hovering = false;
                begin_transition(&mut controller, AutoHidePhase::Revealing, position.y);
            }
            AutoHidePhase::Visible => {
                if gate.voice_active.load(Ordering::Relaxed)
                    || gate.wake_conversation_active.load(Ordering::Relaxed)
                {
                    controller.hovering = false;
                    controller.hide_deadline = None;
                } else if over_window {
                    controller.hovering = true;
                    controller.hide_deadline = None;
                } else {
                    if controller.hovering || controller.hide_deadline.is_none() {
                        controller.hovering = false;
                        controller.hide_deadline =
                            Some(now + Duration::from_millis(AUTO_HIDE_DELAY_MS));
                    }
                    if controller
                        .hide_deadline
                        .is_some_and(|deadline| now >= deadline)
                    {
                        begin_transition(&mut controller, AutoHidePhase::Hiding, position.y);
                    }
                }
            }
            AutoHidePhase::Revealing | AutoHidePhase::Hiding | AutoHidePhase::Hidden => {}
        }
    }

    let mut target_phase = None;
    if let Some(started) = controller.animation_started {
        let elapsed = now.saturating_duration_since(started).as_secs_f64() * 1000.0;
        let progress = (elapsed / ANIMATION_MS as f64).clamp(0.0, 1.0);
        let revealing = controller.phase == AutoHidePhase::Revealing;
        let target_y = if revealing { visible_y } else { hidden_y };
        let eased = if revealing {
            1.0 - (1.0 - progress).powi(2)
        } else {
            progress.powi(2)
        };
        let y = (controller.animation_from_y as f64
            + (target_y - controller.animation_from_y) as f64 * eased)
            .round() as i32;
        let _ = win.set_position(PhysicalPosition::new(position.x, y));
        if progress >= 1.0 {
            let _ = win.set_position(PhysicalPosition::new(position.x, target_y));
            target_phase = Some(if revealing {
                AutoHidePhase::Visible
            } else {
                AutoHidePhase::Hidden
            });
        }
    } else if controller.phase == AutoHidePhase::Hidden && position.y != hidden_y {
        let _ = win.set_position(PhysicalPosition::new(position.x, hidden_y));
    }

    if let Some(phase) = target_phase {
        controller.phase = phase;
        controller.animation_started = None;
        controller.hovering = false;
        controller.hide_deadline = (phase == AutoHidePhase::Visible)
            .then(|| now + Duration::from_millis(AUTO_HIDE_DELAY_MS));
    }
    controller.phase == AutoHidePhase::Hidden
}

/// Position, size and scale of the monitor the island lives on. Any change here
/// means the island has to be placed again.
fn current_screen_key(app: &AppHandle) -> Option<(i32, i32, u32, u32, u64)> {
    let pref = app
        .try_state::<crate::Shared>()
        .map(|s| s.settings.lock().unwrap().screen.clone())
        .unwrap_or_else(|| "primary".into());
    let m = target_monitor(app, &pref)?;
    let p = m.position();
    let size = m.size();
    Some((
        p.x,
        p.y,
        size.width,
        size.height,
        m.scale_factor().to_bits(),
    ))
}

/// Polls Windows globally while auto-hide is enabled, or parks when the
/// renderer has collapsed and auto-hide is disabled.
pub fn spawn_cursor_poll(app: AppHandle, gate: Arc<PollGate>) {
    std::thread::spawn(move || {
        let mut was_down = false;
        // Remembered across wakes so a display change while hidden is noticed the
        // moment the island comes back.
        let mut last_screen: Option<(i32, i32, u32, u32, u64)> = None;
        loop {
            if !gate.should_poll() {
                gate.wait_until_active();
            }
            let mut last = (f64::MIN, f64::MIN);
            let mut ticks: u32 = 0;
            while gate.should_poll() {
                let auto_hide = gate.auto_hide_enabled();
                let phase = gate.auto_hide_phase();
                let collapsed = gate.collapsed.load(Ordering::Relaxed);
                let period = if auto_hide && (phase == AutoHidePhase::Hidden || collapsed) {
                    POLL_INTERVAL_MS
                } else if platform::CURSOR_POLL {
                    ANIMATION_FRAME_MS
                } else {
                    500
                };
                let screen_every = if platform::CURSOR_POLL {
                    (500 / period).max(1) as u32
                } else {
                    1
                };
                std::thread::sleep(Duration::from_millis(period));

                // Monitors get plugged in, unplugged, rearranged and rescaled, and
                // an island pinned to coordinates that no longer exist is an island
                // nobody can reach. Checked about twice a second — the cursor poll
                // is already running, so this costs one monitor query.
                ticks = ticks.wrapping_add(1);
                if ticks % screen_every == 0 {
                    let now = current_screen_key(&app);
                    if now.is_some() && now != last_screen {
                        let first = last_screen.is_none();
                        last_screen = now;
                        if !first {
                            crate::log::line("display layout changed — repositioning".to_string());
                            let _ = app.emit_to(WINDOW_LABEL, "screen-changed", ());
                        }
                    }
                }

                let Some(win) = window(&app) else { break };
                let Some((cx, cy)) = cursor_physical() else {
                    continue;
                };
                let native_hidden = advance_auto_hide(&app, &gate, &win, (cx, cy), Instant::now());
                let Ok(origin) = win.outer_position() else {
                    continue;
                };
                let scale = win.scale_factor().unwrap_or(1.0);
                let x = (cx - origin.x as f64) / scale;
                let y = (cy - origin.y as f64) / scale;
                let size = match win.inner_size() {
                    Ok(s) => (s.width as f64 / scale, s.height as f64 / scale),
                    Err(_) => (PANEL_W, PANEL_H),
                };

                // Notice the drag's initial press even when the cursor has not
                // moved yet; otherwise the WebView2 drop target can win the OLE
                // hit test before wry's parent target is restored.
                let down = left_button_down();
                if down && !was_down {
                    let handle = app.clone();
                    let _ =
                        app.run_on_main_thread(move || platform::unblock_webview_drops(&handle));
                }
                was_down = down;

                if (x - last.0).abs() < 1.0 && (y - last.1).abs() < 1.0 {
                    continue;
                }
                last = (x, y);

                // Click-through: the window only takes the mouse over the island
                // shape. A small entry margin means the flag is already off by the
                // time a moving cursor reaches a button.
                let r = *gate.rect.lock().unwrap();
                let on_island = r.w > 0.0
                    && x >= r.x - HIT_MARGIN
                    && x <= r.x + r.w + HIT_MARGIN
                    && y >= r.y - HIT_MARGIN
                    && y <= r.y + r.h + HIT_MARGIN;

                // A file being dragged has to be able to find us. WS_EX_TRANSPARENT
                // — what click-through is on Windows — hides the window from
                // WindowFromPoint, so OLE finds no drop target and shows the "no
                // drop" cursor. macOS has no such problem: AppKit delivers drags to
                // registered destinations whatever ignoresMouseEvents says. So while
                // a button is held anywhere over the panel, the whole panel takes
                // the mouse, which also makes the drop zone as forgiving as the Mac's.
                // A press may be the start of a drag: make sure the drop target is
                // ours before the file arrives.
                let dragging = down && x >= 0.0 && x <= size.0 && y >= 0.0 && y <= size.1;

                let accept = !native_hidden && (on_island || dragging);
                if gate.ignoring.load(Ordering::Relaxed) == accept {
                    gate.ignoring.store(!accept, Ordering::Relaxed);
                    let _ = win.set_ignore_cursor_events(!accept);
                }

                let _ = win.emit("cursor", CursorPayload { x, y });
            }
        }
    });
}

/// Re-applies click-through after the window or the island changed shape.
///
/// With the cursor poll (Windows) the window takes the mouse again and the next
/// tick decides from the cursor. Without it (Linux) the input region is set to
/// the island itself, or to the whole wake strip while collapsed.
pub fn refresh_click_through(app: &AppHandle, gate: &PollGate) {
    if platform::CURSOR_POLL {
        let hidden = gate.auto_hide_enabled()
            && (gate.auto_hide_phase() == AutoHidePhase::Hidden
                || gate.collapsed.load(Ordering::Relaxed));
        gate.ignoring.store(hidden, Ordering::Relaxed);
        set_ignore_cursor(app, hidden);
        return;
    }
    let Some(win) = window(app) else { return };
    let region = if gate.collapsed.load(Ordering::Relaxed) {
        // The wake strip itself, never "the whole window": if the window ever
        // fails to shrink to the strip, the rest of it must not swallow clicks
        // meant for whatever sits under the top of the screen.
        Some((0.0, 0.0, STRIP_W, STRIP_H))
    } else {
        let r = *gate.rect.lock().unwrap();
        if r.w <= 0.0 {
            // Nothing drawn yet: nothing takes the mouse.
            Some((0.0, 0.0, 0.0, 0.0))
        } else {
            let x0 = (r.x - HIT_MARGIN).max(0.0);
            let y0 = (r.y - HIT_MARGIN).max(0.0);
            let x1 = r.x + r.w + HIT_MARGIN;
            let y1 = r.y + r.h + HIT_MARGIN;
            Some((x0, y0, x1 - x0, y1 - y0))
        }
    };
    platform::set_input_region(&win, region);
}

pub fn set_ignore_cursor(app: &AppHandle, ignore: bool) {
    if let Some(win) = window(app) {
        let _ = win.set_ignore_cursor_events(ignore);
    }
}

#[cfg(test)]
mod tests {
    use super::{prepare_launch_visibility, AutoHidePhase, PollGate, LAUNCH_VISIBLE_MS};
    use std::time::{Duration, Instant};

    #[test]
    fn launch_visibility_starts_visible_with_ten_second_autohide_deadline() {
        let gate = PollGate::new(true);
        prepare_launch_visibility(&gate);
        let after = Instant::now();

        let controller = gate.auto_hide.lock().unwrap();
        assert_eq!(controller.phase, AutoHidePhase::Visible);
        let deadline = controller
            .hide_deadline
            .expect("launch should schedule auto-hide");
        let remaining = deadline.saturating_duration_since(after);
        assert!(remaining <= Duration::from_millis(LAUNCH_VISIBLE_MS));
        assert!(remaining > Duration::from_millis(LAUNCH_VISIBLE_MS - 100));
    }

    #[test]
    fn launch_visibility_keeps_auto_hide_disabled_behavior() {
        let gate = PollGate::new(false);

        prepare_launch_visibility(&gate);

        let controller = gate.auto_hide.lock().unwrap();
        assert_eq!(controller.phase, AutoHidePhase::Visible);
        assert!(controller.hide_deadline.is_none());
    }
}
