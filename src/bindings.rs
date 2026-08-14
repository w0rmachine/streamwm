//! Keybindings: create river xkb bindings from config and dispatch actions
//! when they trigger.

use wayland_client::{Connection as WlConnection, Dispatch, Proxy, QueueHandle};

use crate::connection::AppData;
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
    let mut binds: Vec<(String, String, String)> = vec![
        ("Return".into(), "".into(), "spawn_terminal".into()),
        ("d".into(), "".into(), "spawn_launcher".into()),
        ("q".into(), "".into(), "close".into()),
        ("f".into(), "".into(), "fullscreen".into()),
        ("v".into(), "".into(), "float".into()),
        ("j".into(), "".into(), "focus_next".into()),
        ("k".into(), "".into(), "focus_prev".into()),
        ("space".into(), "".into(), "cycle_layout".into()),
    ];
    for t in 0..=9u32 {
        let key = char::from_digit(t, 10).unwrap().to_string();
        binds.push((key.clone(), "".into(), format!("focus_tag:{t}")));
        binds.push((key, "shift".into(), format!("send_to_tag:{t}")));
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
            if data.config.allow_spawn {
                if let Some(cmd) = arg {
                    crate::wm::spawn::spawn(cmd);
                }
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
                let active = s.outputs[o].active_mask;
                let ids: Vec<u32> = s
                    .windows
                    .iter()
                    .filter(|w| w.output == o && (active >> w.tag) & 1 == 1 && !w.floating)
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
                let mut s = data.state.borrow_mut();
                if let Some(o) = s.active_output() {
                    s.focus_tag(o, tag);
                    needs_manage = true;
                }
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
        "cycle_layout" => {
            // No-op for now; layout is a single tiling layout in v1.
            log::debug!("cycle_layout (single layout; no-op)");
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
        assert!(bindings.iter().any(|(key, modifiers, action)| key == "0"
            && modifiers.is_empty()
            && action == "focus_tag:0"));
        assert!(bindings.iter().any(|(key, modifiers, action)| key == "9"
            && modifiers == "shift"
            && action == "send_to_tag:9"));
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
}
