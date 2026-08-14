//! Window management logic: manage/render sequence handling, layout, focus.

pub mod layout;
pub mod spawn;

use crate::connection::AppData;
use crate::protocols::wm::river_window_manager_v1::RiverWindowManagerV1;

/// Handle a manage sequence: recompute focus/occupancy, then propose
/// dimensions for every visible (non-floating) window.
pub fn on_manage_start(data: &mut AppData, wm: &RiverWindowManagerV1) {
    log::trace!("manage_start");

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
                        w.fullscreen_applied = true;
                    }
                } else {
                    w.proxy.exit_fullscreen();
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
        let state = data.state.borrow();
        for (wid, geom) in geometries {
            if geom.width == 0 || geom.height == 0 {
                continue;
            }
            if let Some(window) = state.find_window(wid) {
                window
                    .proxy
                    .propose_dimensions(geom.width as i32, geom.height as i32);
            }
        }
    }

    // Enable any keybindings that were created before this manage sequence.
    for (binding, _action) in &data.bindings {
        binding.enable();
    }

    wm.manage_finish();
}

/// Handle a render sequence: ensure nodes exist, position windows, set
/// borders, and hide/show.
pub fn on_render_start(data: &mut AppData, wm: &RiverWindowManagerV1) {
    log::trace!("render_start");

    // Ensure every window has its render node (get_node, once).
    {
        let mut state = data.state.borrow_mut();
        let qh = data.qh.clone().expect("qh not set");
        for window in state.windows.iter_mut() {
            if window.node.is_none() {
                let node = window.proxy.get_node(&qh, ());
                window.node = Some(node);
            }
        }
    }

    layout::render_all_run(data);

    wm.render_finish();

    // Refresh the status snapshot for the socket server.
    crate::status::refresh_snapshot(data);
}
