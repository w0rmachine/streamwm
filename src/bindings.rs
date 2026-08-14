//! Keybindings: create river xkb bindings from config and dispatch actions
//! when they trigger.

use wayland_client::{Connection as WlConnection, Dispatch, Proxy, QueueHandle};

use crate::connection::{AppData, OpKind, PointerOp};
use crate::protocols::wm::river_seat_v1::{Modifiers, RiverSeatV1};
use crate::protocols::xkb_bindings::river_xkb_binding_v1::{
    Event as XkbBindingEvent, RiverXkbBindingV1,
};
use crate::protocols::xkb_bindings::river_xkb_bindings_v1::RiverXkbBindingsV1;

/// Parse a keysym name into its xkbcommon keysym value.
fn parse_keysym(name: &str) -> Option<u32> {
    let bytes = name.as_bytes();
    if bytes.len() == 1 {
        let c = bytes[0];
        if c.is_ascii_alphanumeric() {
            return Some(c as u32);
        }
    }
    Some(match name {
        "Return" => 0xff0d,
        "space" => 0x20,
        "Escape" => 0xff1b,
        "Tab" => 0xff09,
        "BackSpace" => 0xff08,
        "Left" => 0xff51,
        "Up" => 0xff52,
        "Right" => 0xff53,
        "Down" => 0xff54,
        "F1" => 0xffbe,
        "F2" => 0xffbf,
        "F3" => 0xffc0,
        "F4" => 0xffc1,
        "F5" => 0xffc2,
        "F6" => 0xffc3,
        "F7" => 0xffc4,
        "F8" => 0xffc5,
        "F9" => 0xffc6,
        "F10" => 0xffc7,
        "F11" => 0xffc8,
        "F12" => 0xffc9,
        "XF86AudioRaiseVolume" => 269025043,
        "XF86AudioLowerVolume" => 269025041,
        "XF86AudioMute" => 269025042,
        "XF86AudioMicMute" => 269025202,
        "XF86MonBrightnessUp" => 269025026,
        "XF86MonBrightnessDown" => 269025027,
        _ => return None,
    })
}

/// Parse a comma-separated modifier list into river's Modifiers bitflags.
fn parse_modifiers(s: &str, config_modifier: &str) -> Modifiers {
    let mut m = Modifiers::empty();
    if s.trim().eq_ignore_ascii_case("none") {
        return m;
    }
    for part in s.split([',', '+']) {
        match part.trim().to_ascii_lowercase().as_str() {
            "" => {}
            "shift" => m |= Modifiers::Shift,
            "ctrl" | "control" => m |= Modifiers::Ctrl,
            "alt" | "mod1" => m |= Modifiers::Mod1,
            "super" | "mod4" | "logo" => m |= Modifiers::Mod4,
            "mod3" => m |= Modifiers::Mod3,
            "mod5" => m |= Modifiers::Mod5,
            _ => {}
        }
    }
    if m.is_empty() {
        // Apply the config default modifier.
        if config_modifier.contains("super") || config_modifier.contains("mod4") {
            m |= Modifiers::Mod4;
        } else if config_modifier.contains("alt") || config_modifier.contains("mod1") {
            m |= Modifiers::Mod1;
        } else if config_modifier.contains("ctrl") {
            m |= Modifiers::Ctrl;
        }
    }
    m
}

/// Build the list of (keysym, modifiers, action) bindings.
pub fn default_bindings(config: &crate::config::Config) -> Vec<(String, String, String)> {
    let modified_shift = format!("{}+shift", config.modifier);
    let mut binds: Vec<(String, String, String)> = vec![
        ("Return".into(), "".into(), "spawn_terminal".into()),
        ("d".into(), "".into(), "spawn_launcher".into()),
        ("q".into(), "".into(), "close".into()),
        ("E".into(), config.modifier.clone(), "quit".into()),
        ("f".into(), "".into(), "fullscreen".into()),
        ("v".into(), "".into(), "float".into()),
        ("j".into(), "".into(), "focus_next".into()),
        ("k".into(), "".into(), "focus_prev".into()),
        ("h".into(), "".into(), "focus_prev_output".into()),
        ("l".into(), "".into(), "focus_next_output".into()),
        ("space".into(), "".into(), "cycle_layout".into()),
        ("r".into(), "".into(), "enter_resize_mode".into()),
        ("Left".into(), "none".into(), "resize_step_left".into()),
        ("Right".into(), "none".into(), "resize_step_right".into()),
        ("h".into(), "none".into(), "resize_step_left".into()),
        ("l".into(), "none".into(), "resize_step_right".into()),
        ("Escape".into(), "none".into(), "exit_resize_mode".into()),
    ];
    for t in 1..=9u32 {
        let key = char::from_digit(t, 10).unwrap().to_string();
        let tag = t - 1;
        binds.push((key.clone(), "".into(), format!("focus_tag:{tag}")));
        binds.push((key, modified_shift.clone(), format!("send_to_tag:{tag}")));
    }
    for b in &config.bindings {
        let action = match &b.arg {
            Some(arg) if !arg.is_empty() && !b.action.contains(':') => {
                format!("{}:{arg}", b.action)
            }
            _ => b.action.clone(),
        };
        binds.push((b.keysym.clone(), b.modifiers.clone(), action));
    }
    binds
}

/// Bind all configured bindings for a seat.
pub fn bind_for_seat(data: &mut AppData, xkb: &RiverXkbBindingsV1, seat: &RiverSeatV1) {
    let qh = data.qh.clone().expect("qh not set");
    let modifier = data.config.modifier.clone();
    let binds = default_bindings(&data.config);

    for (keysym, mods, action) in binds {
        let Some(ks) = parse_keysym(&keysym) else {
            log::warn!("unknown keysym `{keysym}`, skipping binding");
            continue;
        };
        let m = parse_modifiers(&mods, &modifier);
        let binding = xkb.get_xkb_binding(seat, ks, m, &qh, ());
        data.bindings.push((binding, action));
    }
}

/// Linux input event codes for the two mouse buttons we bind.
const BTN_LEFT: u32 = 0x110; // 272
const BTN_RIGHT: u32 = 0x111; // 273

/// Bind Mod+left-drag (move) and Mod+right-drag (resize) for floating windows.
pub fn bind_pointer_for_seat(data: &mut AppData, seat: &RiverSeatV1) {
    let qh = data.qh.clone().expect("qh not set");
    let modifier = data.config.modifier.clone();
    let m = parse_modifiers("", &modifier);

    let b_move = seat.get_pointer_binding(BTN_LEFT, m, &qh, ());
    let b_resize = seat.get_pointer_binding(BTN_RIGHT, m, &qh, ());
    data.pointer_bindings.push((b_move, "move".into()));
    data.pointer_bindings.push((b_resize, "resize".into()));
}

/// Dispatch a triggered binding to its action.
pub fn dispatch_action(data: &mut AppData, triggered: &RiverXkbBindingV1) {
    // Find the action by comparing proxy ids.
    let action = data
        .bindings
        .iter()
        .find(|(b, _)| b.id() == triggered.id())
        .map(|(_, a)| a.clone());

    let Some(action) = action else {
        log::warn!("triggered unknown binding");
        return;
    };

    run_action(data, &action);
}

/// Whether an action is a resize-mode binding that should only be active while
/// resize mode is entered. These use the `none` modifier, so if they were left
/// enabled permanently they would swallow the plain keys (h/l/arrows/Escape)
/// across the whole session.
pub fn is_resize_binding(action: &str) -> bool {
    matches!(
        action,
        "resize_step_left" | "resize_step_right" | "exit_resize_mode"
    )
}

fn run_action(data: &mut AppData, action: &str) {
    log::debug!("action: {action}");

    // Split off optional `:arg`.
    let (name, arg) = match action.split_once(':') {
        Some((n, a)) => (n, Some(a)),
        None => (action, None),
    };

    // Whether this action changed window management state and needs a manage
    // sequence. Protocol requests (close/fullscreen/focus/propose_dimensions)
    // may only be made inside a manage sequence, so we defer them to
    // on_manage_start and request one here via manage_dirty.
    let mut needs_manage = false;

    match name {
        "spawn_terminal" => crate::wm::spawn::spawn(&data.config.terminal),
        "spawn_launcher" => crate::wm::spawn::spawn(&data.config.launcher),
        "spawn" => {
            // Keybindings fire only on a physical keypress, so a config-declared
            // `spawn` binding is trusted. The `allow_spawn` guard governs only
            // the *socket* control path (status.rs), where arbitrary local
            // processes can inject commands.
            if let Some(cmd) = arg {
                crate::wm::spawn::spawn(cmd);
            }
        }
        "close" => {
            let fid = {
                let s = data.state.borrow();
                s.active_output().and_then(|o| s.outputs[o].focused_window)
            };
            if let Some(fid) = fid {
                data.pending_close.push(fid);
                needs_manage = true;
            }
        }
        "focus_next" | "focus_prev" => {
            let mut s = data.state.borrow_mut();
            if let Some(o) = s.active_output() {
                let active = s.outputs[o].active_tag;
                let ids: Vec<u32> = s
                    .windows
                    .iter()
                    .filter(|w| s.tag_owner(w.tag) == Some(o) && w.tag == active && !w.floating)
                    .map(|w| w.id)
                    .collect();
                if !ids.is_empty() {
                    let cur = s.outputs[o].focused_window;
                    let idx = cur
                        .and_then(|f| ids.iter().position(|i| *i == f))
                        .unwrap_or(0);
                    let next = if name == "focus_next" {
                        (idx + 1) % ids.len()
                    } else {
                        (idx + ids.len() - 1) % ids.len()
                    };
                    s.outputs[o].focused_window = Some(ids[next]);
                    needs_manage = true;
                }
            }
        }
        "focus_tag" => {
            if let Some(tag) = arg.and_then(|t| t.parse::<usize>().ok()) {
                // If this tag lives on a different output, focusing it moves
                // focus (and the pointer) to that output.
                let target = {
                    let s = data.state.borrow();
                    s.active_output()
                        .and_then(|o| s.tag_owner(tag).filter(|&owner| owner != o))
                };
                {
                    let mut s = data.state.borrow_mut();
                    if let Some(o) = s.active_output() {
                        s.focus_tag(o, tag);
                        needs_manage = true;
                    }
                }
                data.pending_pointer_warp = target;
            }
        }
        "send_to_tag" => {
            if let Some(tag) = arg.and_then(|t| t.parse::<usize>().ok()) {
                let mut s = data.state.borrow_mut();
                if let Some(o) = s.active_output() {
                    s.send_focused_to_tag(o, tag);
                    needs_manage = true;
                }
            }
        }
        "fullscreen" => {
            // Toggle desired state; the protocol request is applied in
            // on_manage_start.
            let mut s = data.state.borrow_mut();
            if let Some(o) = s.active_output() {
                if let Some(fid) = s.outputs[o].focused_window {
                    if let Some(w) = s.find_window_mut(fid) {
                        w.fullscreen = !w.fullscreen;
                        needs_manage = true;
                    }
                }
            }
        }
        "float" => {
            let mut s = data.state.borrow_mut();
            if let Some(o) = s.active_output() {
                if let Some(fid) = s.outputs[o].focused_window {
                    if let Some(w) = s.find_window_mut(fid) {
                        w.floating = !w.floating;
                        needs_manage = true;
                    }
                }
            }
        }
        "focus_next_output" | "focus_prev_output" => {
            // Move seat focus to the next/previous output (by index), including
            // empty outputs. Without this, an output with no windows can never
            // gain focus, so newly spawned windows always land on the first
            // output that ever had focus.
            let next = {
                let mut s = data.state.borrow_mut();
                let n = s.outputs.len();
                if n > 1 {
                    let cur = s.focused_output.unwrap_or(0);
                    let next = if name == "focus_next_output" {
                        (cur + 1) % n
                    } else {
                        (cur + n - 1) % n
                    };
                    s.focused_output = Some(next);
                    Some(next)
                } else {
                    None
                }
            };
            if let Some(next) = next {
                data.pending_pointer_warp = Some(next);
                needs_manage = true;
            }
        }
        "cycle_layout" => {
            // No-op for now; layout is a single tiling layout in v1.
            log::debug!("cycle_layout (single layout; no-op)");
        }
        "enter_resize_mode" => {
            data.state.borrow_mut().resize_mode = true;
            // Re-enable the resize-mode-only keybindings (h/l/arrows/Escape)
            // via a manage sequence.
            needs_manage = true;
            log::debug!("resize mode on");
        }
        "exit_resize_mode" => {
            data.state.borrow_mut().resize_mode = false;
            // Disable the resize-mode-only keybindings so the plain keys are
            // delivered to focused surfaces again.
            needs_manage = true;
            log::debug!("resize mode off");
        }
        "resize_step_left" | "resize_step_right" => {
            let mut s = data.state.borrow_mut();
            if !s.resize_mode {
                return;
            }
            let step = data.config.resize_step;
            let delta = if name == "resize_step_right" { step } else { -step };
            s.master_fraction = (s.master_fraction + delta).clamp(0.1, 0.9);
            needs_manage = true;
        }
        "quit" => {
            data.quit = true;
        }
        _ => {
            log::debug!("unhandled action: {name}");
        }
    }

    if needs_manage {
        if let Some(wm) = &data.wm {
            wm.manage_dirty();
        }
    }
}

impl Dispatch<RiverXkbBindingV1, ()> for AppData {
    fn event(
        data: &mut Self,
        binding: &RiverXkbBindingV1,
        event: XkbBindingEvent,
        _ud: &(),
        _conn: &WlConnection,
        _qh: &QueueHandle<Self>,
    ) {
        if let XkbBindingEvent::Pressed = event {
            dispatch_action(data, binding);
        }
    }
}

impl Dispatch<
    crate::protocols::wm::river_pointer_binding_v1::RiverPointerBindingV1,
    (),
> for AppData
{
    fn event(
        data: &mut Self,
        binding: &crate::protocols::wm::river_pointer_binding_v1::RiverPointerBindingV1,
        event: crate::protocols::wm::river_pointer_binding_v1::Event,
        _ud: &(),
        _conn: &WlConnection,
        _qh: &QueueHandle<Self>,
    ) {
        use crate::protocols::wm::river_pointer_binding_v1::Event as PEvent;
        match event {
            PEvent::Pressed => start_pointer_op(data, binding),
            PEvent::Released => end_pointer_op(data),
        }
    }
}

/// Start a move/resize operation for the floating window under the pointer.
fn start_pointer_op(
    data: &mut AppData,
    binding: &crate::protocols::wm::river_pointer_binding_v1::RiverPointerBindingV1,
) {
    let action = data
        .pointer_bindings
        .iter()
        .find(|(b, _)| b.id() == binding.id())
        .map(|(_, a)| a.clone());
    let Some(action) = action else {
        return;
    };

    // Find the floating window under the pointer and its seat.
    let (wid, seat, px, py) = {
        let state = data.state.borrow();
        let mut found = None;
        for seat in state.seats.iter() {
            if let Some(wid) = seat.pointer_window {
                if state.find_window(wid).map(|w| w.floating).unwrap_or(false) {
                    found = Some((wid, seat.proxy.clone(), seat.pointer_x, seat.pointer_y));
                    break;
                }
            }
        }
        let Some(found) = found else {
            return;
        };
        found
    };

    let (fx, fy, fw, fh) = {
        let state = data.state.borrow();
        match state.find_window(wid) {
            Some(w) => (w.float_x, w.float_y, w.float_w, w.float_h),
            None => return,
        }
    };

    let kind = if action == "move" {
        OpKind::Move
    } else {
        OpKind::Resize
    };

    // Defer op_start_pointer to the manage sequence (it modifies window
    // management state).
    data.pending_op = Some(PointerOp {
        window: wid,
        kind,
        start_x: px,
        start_y: py,
        start_float_x: fx,
        start_float_y: fy,
        start_w: fw,
        start_h: fh,
        seat,
    });
    if let Some(wm) = &data.wm {
        wm.manage_dirty();
    }
}

/// Apply a cumulative pointer delta to the active operation.
pub fn apply_pointer_delta(data: &mut AppData, dx: i32, dy: i32) {
    let Some(op) = data.pointer_op.as_ref() else {
        return;
    };
    let (window, kind, sx, sy, sfx, sfy, sw, sh) = (
        op.window,
        op.kind,
        op.start_x,
        op.start_y,
        op.start_float_x,
        op.start_float_y,
        op.start_w,
        op.start_h,
    );

    let mut state = data.state.borrow_mut();
    let Some(w) = state.find_window_mut(window) else {
        return;
    };
    match kind {
        OpKind::Move => {
            w.float_x = sfx + (dx - sx);
            w.float_y = sfy + (dy - sy);
        }
        OpKind::Resize => {
            let nw = (sw as i32 + (dx - sx)).max(50);
            let nh = (sh as i32 + (dy - sy)).max(50);
            w.float_w = nw as u32;
            w.float_h = nh as u32;
        }
    }
}

/// End the active pointer operation and send op_end to the driving seat.
pub fn end_pointer_op(data: &mut AppData) {
    // Defer op_end to the manage sequence (it modifies window management
    // state).
    if data.pointer_op.is_some() || data.pending_op.is_some() {
        data.op_end_requested = true;
        if let Some(wm) = &data.wm {
            wm.manage_dirty();
        }
    }
}

/// Process queued pointer op start/end requests, called inside a manage
/// sequence. Must be invoked between manage_start and manage_finish.
pub fn process_pointer_ops(data: &mut AppData) {
    // Start a queued op.
    if let Some(op) = data.pending_op.take() {
        op.seat.op_start_pointer();
        data.pointer_op = Some(op);
    }
    // End the active op if requested.
    if data.op_end_requested {
        data.op_end_requested = false;
        if let Some(op) = data.pointer_op.take() {
            op.seat.op_end();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Binding, Config};

    #[test]
    fn configured_binding_arg_becomes_action_suffix() {
        let mut config = Config::default();
        config.bindings.push(Binding {
            keysym: "F1".into(),
            modifiers: "shift".into(),
            action: "spawn".into(),
            arg: Some("foot".into()),
        });

        assert!(default_bindings(&config)
            .iter()
            .any(|(key, modifiers, action)| key == "F1"
                && modifiers == "shift"
                && action == "spawn:foot"));
    }

    #[test]
    fn configured_binding_with_inline_arg_is_not_double_suffixed() {
        let mut config = Config::default();
        config.bindings.push(Binding {
            keysym: "F2".into(),
            modifiers: "".into(),
            action: "focus_tag:3".into(),
            arg: Some("9".into()),
        });

        assert!(default_bindings(&config)
            .iter()
            .any(|(_, _, action)| action == "focus_tag:3"));
        assert!(!default_bindings(&config)
            .iter()
            .any(|(_, _, action)| action == "focus_tag:3:9"));
    }

    #[test]
    fn default_bindings_cover_tags_and_core_actions() {
        let bindings = default_bindings(&Config::default());

        assert!(bindings
            .iter()
            .any(|(_, _, action)| action == "spawn_terminal"));
        assert!(bindings.iter().any(|(_, _, action)| action == "focus_next"));
        assert!(bindings.iter().any(|(key, modifiers, action)| key == "E"
            && modifiers == "super"
            && action == "quit"));
        assert!(bindings.iter().any(|(key, modifiers, action)| key == "1"
            && modifiers.is_empty()
            && action == "focus_tag:0"));
        assert!(bindings.iter().any(|(key, modifiers, action)| key == "9"
            && modifiers == "super+shift"
            && action == "send_to_tag:8"));
    }

    #[test]
    fn keysym_parser_handles_known_and_unknown_values() {
        assert_eq!(parse_keysym("a"), Some('a' as u32));
        assert_eq!(parse_keysym("Return"), Some(0xff0d));
        assert_eq!(parse_keysym("XF86AudioMute"), Some(269025042));
        assert_eq!(parse_keysym("NotAKeysym"), None);
    }

    #[test]
    fn modifier_parser_accepts_commas_plus_and_config_default() {
        let explicit = parse_modifiers("shift+ctrl,alt", "super");
        assert!(explicit.contains(Modifiers::Shift));
        assert!(explicit.contains(Modifiers::Ctrl));
        assert!(explicit.contains(Modifiers::Mod1));
        assert!(!explicit.contains(Modifiers::Mod4));

        assert!(parse_modifiers("", "super").contains(Modifiers::Mod4));
        assert!(parse_modifiers("", "alt").contains(Modifiers::Mod1));
        assert!(parse_modifiers("", "ctrl").contains(Modifiers::Ctrl));
    }

    #[test]
    fn modifier_parser_accepts_none() {
        assert!(parse_modifiers("none", "super").is_empty());
    }

    #[test]
    fn default_bindings_include_resize_mode() {
        let bindings = default_bindings(&Config::default());

        assert!(bindings.iter().any(|(key, modifiers, action)| key == "r"
            && modifiers.is_empty()
            && action == "enter_resize_mode"));
        assert!(bindings.iter().any(|(key, modifiers, action)| key == "Left"
            && modifiers == "none"
            && action == "resize_step_left"));
        assert!(bindings.iter().any(|(key, modifiers, action)| key == "Right"
            && modifiers == "none"
            && action == "resize_step_right"));
        assert!(bindings.iter().any(|(key, modifiers, action)| key == "Escape"
            && modifiers == "none"
            && action == "exit_resize_mode"));
    }

    #[test]
    fn resize_bindings_are_flagged_resize_only() {
        assert!(is_resize_binding("resize_step_left"));
        assert!(is_resize_binding("resize_step_right"));
        assert!(is_resize_binding("exit_resize_mode"));
        assert!(!is_resize_binding("enter_resize_mode"));
        assert!(!is_resize_binding("focus_next"));
        assert!(!is_resize_binding("spawn"));
    }
}
