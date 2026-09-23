//! Window management state transition handlers (`manage` and `render` sequences).
//!
//! Enforces River compositor protocol requirements:
//! - Window management state operations (`propose_dimensions`, `focus_window`, `close`, `fullscreen`, `use_ssd`/`use_csd`, binding enable/disable) occur strictly inside `manage_start`/`manage_finish`.
//! - Rendering state operations (`set_position`, `place_top`, `set_borders`, `show`, `hide`) occur inside `render_start`/`render_finish`.

pub mod layout;
pub mod spawn;

use crate::connection::AppData;
use crate::protocols::wm::river_window_manager_v1::RiverWindowManagerV1;

/// Executes a River manage sequence: processes pointer operations, applies queued window closing/fullscreen requests,
/// routes keyboard seat focus, computes tiling layout geometries, proposes window content dimensions,
/// enables/disables hotkeys based on modal resize state, and warps pointer cursor on cross-output switches.
pub fn on_manage_start(data: &mut AppData, wm: &RiverWindowManagerV1) {
    let started = std::time::Instant::now();
    log::trace!("manage_start");

    // Process queued interactive pointer operations (op_start_pointer /
    // op_end) which must be issued inside a manage sequence.
    crate::bindings::process_pointer_ops(data);

    {
        let mut state = data.state.borrow_mut();
        for output_idx in 0..state.outputs.len() {
            state.refocus_output(output_idx);
        }
    }

    // Newly created windows are shown by default in the river protocol. Keep
    // every window hidden until the render sequence shows the final visible set.
    {
        let state = data.state.borrow();
        for window in state.windows.iter() {
            window.proxy.hide();
        }
    }

    // Apply deferred close requests (window management state, so this must
    // happen inside a manage sequence).
    let pending_close = std::mem::take(&mut data.pending_close);
    {
        let state = data.state.borrow();
        for fid in pending_close {
            if let Some(w) = state.find_window(fid) {
                w.proxy.close();
            }
        }
    }

    // Apply pending fullscreen state changes (window management state).
    {
        let mut state = data.state.borrow_mut();
        let outputs: Vec<_> = state
            .outputs
            .iter()
            .map(|output| output.proxy.clone())
            .collect();
        let window_outputs: Vec<usize> = state
            .windows
            .iter()
            .map(|w| state.tag_owner(w.tag).unwrap_or(0))
            .collect();
        for (i, w) in state.windows.iter_mut().enumerate() {
            if !w.caps_set {
                // Declare the window-management capabilities streamwm actually
                // honors so apps hide unsupported buttons. We support maximize
                // (mapped to fullscreen), fullscreen, and minimize (hide); we
                // do not show a window menu.
                use crate::protocols::wm::river_window_v1::Capabilities;
                let caps = Capabilities::Maximize
                    | Capabilities::Fullscreen
                    | Capabilities::Minimize;
                w.proxy.set_capabilities(caps);
                w.caps_set = true;
            }

            if w.ssd_applied != Some(data.config.use_ssd) {
                if data.config.use_ssd {
                    w.proxy.use_ssd();
                } else {
                    w.proxy.use_csd();
                }
                w.ssd_applied = Some(data.config.use_ssd);
            }

            if w.fullscreen != w.fullscreen_applied {
                if w.fullscreen {
                    if let Some(output) = outputs.get(window_outputs[i]) {
                        w.proxy.fullscreen(output);
                        // Inform the app of both its maximized and fullscreen
                        // state so CSD titlebars update accordingly; a tiling
                        // WM's maximize and fullscreen are the same outcome.
                        w.proxy.inform_maximized();
                        w.proxy.inform_fullscreen();
                        w.fullscreen_applied = true;
                    }
                } else {
                    w.proxy.exit_fullscreen();
                    w.proxy.inform_unmaximized();
                    w.proxy.inform_not_fullscreen();
                    w.fullscreen_applied = false;
                }
            }
        }
    }

    // Set a default layer-shell output once (required so clients with no
    // explicit output preference can map their surfaces).
    if !data.layer_default_set {
        let state = data.state.borrow();
        if let Some(out) = state.outputs.first() {
            if let Some(layer) = &out.layer {
                layer.set_default();
                data.layer_default_set = true;
            }
        }
    }

    // Route keyboard focus to the focused window of the focused output.
    {
        let state = data.state.borrow();
        let focused_proxy = state.active_output().and_then(|o| {
            state.outputs[o]
                .focused_window
                .and_then(|fid| state.find_window(fid))
                .map(|w| w.proxy.clone())
        });
        for seat in state.seats.iter() {
            if let Some(ref proxy) = focused_proxy {
                seat.proxy.focus_window(proxy);
            } else {
                seat.proxy.clear_focus();
            }
        }
    }

    // Compute geometries (immutable borrow), then propose (mutable borrow).
    let geometries = {
        let state = data.state.borrow();
        layout::compute_all(&state, &data.config)
    };

    {
        let mut state = data.state.borrow_mut();
        let mut proposed = 0usize;
        for (wid, geom) in geometries {
            if geom.width == 0 || geom.height == 0 {
                continue;
            }
            if let Some(window) = state.find_window_mut(wid) {
                if window.proposed_dimensions == Some((geom.width, geom.height)) {
                    continue;
                }
                window
                    .proxy
                    .propose_dimensions(geom.width as i32, geom.height as i32);
                window.proposed_dimensions = Some((geom.width, geom.height));
                proposed += 1;
            }
        }
        // Propose dimensions for floating windows (their float_w/float_h, set
        // by interactive resize).
        for window in state.windows.iter_mut().filter(|w| w.floating) {
            if window.float_w > 0 && window.float_h > 0 {
                if window.proposed_dimensions == Some((window.float_w, window.float_h)) {
                    continue;
                }
                window
                    .proxy
                    .propose_dimensions(window.float_w as i32, window.float_h as i32);
                window.proposed_dimensions = Some((window.float_w, window.float_h));
                proposed += 1;
            }
        }
        log::debug!("manage proposed {proposed} window dimensions");
    }

    // Enable keybindings that were created before this manage sequence.
    // Resize-mode bindings (h/l/arrows/Escape, bound with the `none` modifier)
    // are only enabled while resize mode is active; otherwise they would
    // swallow those plain keys everywhere.
    let resize_mode = data.state.borrow().resize_mode;
    for (binding, action) in &data.bindings {
        if crate::bindings::is_resize_binding(action) {
            if resize_mode {
                binding.enable();
            } else {
                binding.disable();
            }
        } else {
            binding.enable();
        }
    }
    for (binding, _action) in &data.pointer_bindings {
        binding.enable();
    }

    // Warp the pointer to the target output after a keyboard-driven output or
    // cross-output tag focus, so focus-follows-mouse does not snap focus back
    // to the output the pointer is still on.
    if let Some(target) = data.pending_pointer_warp.take() {
        let state = data.state.borrow();
        if let Some(output) = state.outputs.get(target) {
            let cx = output.x + output.width as i32 / 2;
            let cy = output.y + output.height as i32 / 2;
            for seat in state.seats.iter() {
                seat.proxy.pointer_warp(cx, cy);
            }
        }
    }

    wm.manage_finish();
    log::debug!("manage_start finished in {:?}", started.elapsed());
}

/// Handle a render sequence: ensure nodes exist, position windows, set
/// borders, and hide/show.
pub fn on_render_start(data: &mut AppData, wm: &RiverWindowManagerV1) {
    let started = std::time::Instant::now();
    log::trace!("render_start");

    // Ensure visible windows have their render node (get_node, once).
    {
        let mut state = data.state.borrow_mut();
        let qh = data.qh.clone().expect("qh not set");
        let visible: Vec<bool> = state
            .windows
            .iter()
            .map(|window| state.window_is_visible(window))
            .collect();
        let mut requested = 0usize;
        for (window, visible) in state.windows.iter_mut().zip(visible) {
            if window.node.is_none() {
                if !visible {
                    continue;
                }
                let node = window.proxy.get_node(&qh, ());
                window.node = Some(node);
                requested += 1;
            }
        }
        log::debug!("render requested {requested} window nodes");
    }

    layout::render_all_run(data);

    wm.render_finish();

    // Refresh the status snapshot for the socket server.
    crate::status::refresh_snapshot(data);
    log::debug!("render_start finished in {:?}", started.elapsed());
}
