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
