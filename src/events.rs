//! Event dispatch implementations for River protocol interfaces:
//! - `RiverWindowV1`: Closed, AppId, Title, Dimensions
//! - `RiverOutputV1`: Position, Dimensions, WlOutput binding, Removed
//! - `RiverLayerShellOutputV1`: NonExclusiveArea
//! - `RiverSeatV1`: PointerEnter, PointerLeave, PointerPosition, WindowInteraction, OpDelta, OpRelease, Removed
//! - `WlOutput`: Name

use wayland_client::{
    protocol::{
        wl_output::{Event as WlOutputEvent, WlOutput},
        wl_seat::WlSeat,
    },
    Connection as WlConnection, Dispatch, Proxy, QueueHandle,
};

use crate::connection::AppData;
use crate::protocols::layer_shell::river_layer_shell_output_v1::{
    Event as LayerOutputEvent, RiverLayerShellOutputV1,
};
use crate::protocols::wm::river_output_v1::{Event as OutputEvent, RiverOutputV1};
use crate::protocols::wm::river_seat_v1::{Event as SeatEvent, RiverSeatV1};
use crate::protocols::wm::river_window_v1::{Event as WindowEvent, RiverWindowV1};
use crate::state::State;

/// Dispatch implementation for window events dispatched by `RiverWindowV1`.
impl Dispatch<RiverWindowV1, ()> for AppData {
    fn event(
        data: &mut Self,
        window: &RiverWindowV1,
        event: WindowEvent,
        _ud: &(),
        _conn: &WlConnection,
        _qh: &QueueHandle<Self>,
    ) {
        let oid = window.id();
        let mut needs_manage = false;

        // Interactive move/resize requests mutate op + float state through
        // `start_interactive_op`, which needs `&mut data`; handle them before
        // taking the general `state` borrow.
        match &event {
            WindowEvent::PointerMoveRequested { seat } => {
                let wid = data.state.borrow().find_window_by_proxy(window);
                if let Some(wid) = wid {
                    crate::bindings::start_interactive_op(
                        data,
                        wid,
                        seat.clone(),
                        crate::connection::OpKind::Move,
                        crate::protocols::wm::river_window_v1::Edges::empty(),
                    );
                }
                return;
            }
            WindowEvent::PointerResizeRequested { seat, edges } => {
                let wid = data.state.borrow().find_window_by_proxy(window);
                if let Some(wid) = wid {
                    let edges = edges
                        .into_result()
                        .unwrap_or_else(|_| crate::protocols::wm::river_window_v1::Edges::empty());
                    crate::bindings::start_interactive_op(
                        data,
                        wid,
                        seat.clone(),
                        crate::connection::OpKind::Resize,
                        edges,
                    );
                }
                return;
            }
            _ => {}
        }

        let mut state = data.state.borrow_mut();
        let Some(id) = state.find_window_by_proxy(window) else {
            return;
        };
        match event {
            WindowEvent::Closed => {
                if let Some(w) = state.find_window(id) {
                    log::info!("window closed id={} app_id={:?} title={:?}", w.id, w.app_id, w.title);
                    if let Some(node) = &w.node {
                        node.destroy();
                    }
                    w.proxy.destroy();
                }
                state.windows.retain(|w| w.proxy.id() != oid);
            }
            WindowEvent::AppId { app_id } => {
                log::info!("window app_id id={id} app_id={app_id:?}");
                // Auto-float windows whose app id is in the configured
                // "never tile" list (password forms, calculator, Google
                // Meet call window, ...). Seed a centered floating position on
                // the window's output so it doesn't appear at the global
                // origin (output 0).
                let should_float = app_id
                    .as_deref()
                    .is_some_and(|a| data.config.is_floating_app(a));
                let area = if should_float {
                    state
                        .find_window(id)
                        .and_then(|w| state.window_output(w))
                        .and_then(|o| state.outputs.get(o))
                        .map(crate::state::output_area)
                } else {
                    None
                };
                if let Some(w) = state.find_window_mut(id) {
                    if should_float && !w.floating {
                        w.floating = true;
                        // Fit inside the panel-free area: some floating apps
                        // (password prompts, portals) request an output-sized
                        // surface, which would then be clamped to the corner.
                        if let Some((ax, ay, aw, ah)) = area {
                            let (fw, fh) = crate::state::fit_beside_parent(
                                (aw, ah),
                                (w.float_w, w.float_h),
                                (w.min_width, w.min_height, w.max_width, w.max_height),
                            );
                            w.float_w = fw;
                            w.float_h = fh;
                            let (x, y) =
                                crate::state::place_over_parent(fw, fh, ax, ay, aw, ah, (ax, ay, aw, ah));
                            w.float_x = x;
                            w.float_y = y;
                        }
                    }
                    w.app_id = app_id;
                }
                state.mark_dirty();
            }
            WindowEvent::Title { title } => {
                if let Some(w) = state.find_window_mut(id) {
                    w.title = title;
                }
            }
            WindowEvent::Dimensions { width, height } => {
                if let Some(w) = state.find_window_mut(id) {
                    w.width = width as u32;
                    w.height = height as u32;
                }
            }
            WindowEvent::DimensionsHint {
                min_width,
                min_height,
                max_width,
                max_height,
            } => {
                // Store the app's preferred min/max so propose_dimensions can
                // clamp to them in on_manage_start.
                if let Some(w) = state.find_window_mut(id) {
                    w.min_width = min_width.max(0) as u32;
                    w.min_height = min_height.max(0) as u32;
                    w.max_width = max_width.max(0) as u32;
                    w.max_height = max_height.max(0) as u32;
                }
            }
            WindowEvent::Parent { parent } => {
                // A transient (dialog/file picker) set a parent. Record it and
                // float the child over the parent if the parent is floating, so
                // dialogs stay grouped with their owner instead of tiling flat.
                let parent_geom = parent.as_ref().and_then(|p| {
                    let pid = p.id();
                    state
                        .windows
                        .iter()
                        .find(|w| w.proxy.id() == pid)
                        .map(|w| {
                            let area = state
                                .window_output(w)
                                .and_then(|o| state.outputs.get(o))
                                .map(crate::state::output_area)
                                .unwrap_or((w.float_x, w.float_y, w.float_w, w.float_h));
                            (w.id, w.floating, w.float_x, w.float_y, w.float_w, w.float_h, area)
                        })
                });
                if let Some((pid, floating, px, py, pw, ph, area)) = parent_geom {
                    let child = state
                        .find_window(id)
                        .map(|w| (w.min_width, w.min_height, w.max_width, w.max_height))
                        .unwrap_or_default();
                    let Some(w) = state.find_window_mut(id) else {
                        return;
                    };
                    let (min_w, min_h, max_w, max_h) = child;
                    w.parent = Some(pid);
                    if floating && !w.floating {
                        // Overlay the dialog on its floating parent, sized so
                        // the parent stays visible around it (GPS-size dialogs
                        // and portals otherwise cover it completely).
                        let (fw, fh) = crate::state::fit_beside_parent(
                            (pw, ph),
                            (w.float_w, w.float_h),
                            (min_w, min_h, max_w, max_h),
                        );
                        w.floating = true;
                        w.float_w = fw;
                        w.float_h = fh;
                        let (x, y) = crate::state::place_over_parent(fw, fh, px, py, pw, ph, area);
                        w.float_x = x;
                        w.float_y = y;
                    }
                }
                state.mark_dirty();
            }
            WindowEvent::FullscreenRequested { .. } => {
                // Honor a client-initiated fullscreen request (e.g. the `f`/
                // `F11` key inside a video player or browser). The actual
                // protocol `fullscreen` request is applied in the next manage
                // sequence (on_manage_start), matching the WM's own toggle.
                if let Some(w) = state.find_window_mut(id) {
                    log::info!("fullscreen requested id={} app_id={:?} title={:?}", w.id, w.app_id, w.title);
                    w.fullscreen = true;
                    needs_manage = true;
                }
            }
            WindowEvent::ExitFullscreenRequested => {
                if let Some(w) = state.find_window_mut(id) {
                    log::info!("exit fullscreen requested id={} app_id={:?} title={:?}", w.id, w.app_id, w.title);
                    w.fullscreen = false;
                    needs_manage = true;
                }
            }
            WindowEvent::MaximizeRequested => {
                // A tiling WM already fills each window's tile, so there is no
                // distinct "maximize" state; mapping it to fullscreen makes
                // apps that eagerly send maximize_requested unexpectedly cover
                // the whole output (and other tags). Ignore it.
            }
            WindowEvent::UnmaximizeRequested => {
                // Ignored for the same reason: no tiling "maximize" state.
            }
            WindowEvent::MinimizeRequested => {
                // Hide the window until it is interacted with again; streamwm
                // has no minimize state, so hidden + tag focus is the analog.
                if let Some(w) = state.find_window_mut(id) {
                    w.minimized = true;
                    needs_manage = true;
                }
            }
            _ => {}
        }
        drop(state);
        if needs_manage {
            request_manage(data);
        }
    }
}

/// Dispatch implementation for physical output events dispatched by `RiverOutputV1`.
impl Dispatch<RiverOutputV1, ()> for AppData {
    fn event(
        data: &mut Self,
        output: &RiverOutputV1,
        event: OutputEvent,
        _ud: &(),
        _conn: &WlConnection,
        _qh: &QueueHandle<Self>,
    ) {
        let oid = output.id();
        let mut state = data.state.borrow_mut();
        // Output indices shift when one is removed, so every arm re-finds its
        // output by object id instead of holding an index across mutations.
        let idx = || state.outputs.iter().position(|o| o.proxy.id() == oid);
        let mut changed = false;
        match event {
            OutputEvent::Position { x, y } => {
                if let Some(idx) = idx() {
                    state.outputs[idx].x = x;
                    state.outputs[idx].y = y;
                    state.repair_outputs();
                    changed = true;
                }
            }
            OutputEvent::Dimensions { width, height } => {
                if let Some(idx) = idx() {
                    state.outputs[idx].width = width as u32;
                    state.outputs[idx].height = height as u32;
                    state.repair_outputs();
                    changed = true;
                }
            }
            OutputEvent::WlOutput { name } => {
                if let (Some(registry), Some(qh)) = (data.registry.clone(), data.qh.clone()) {
                    let wl_output = registry.bind::<WlOutput, _, _>(name, 4, &qh, name);
                    if let Some(idx) = idx() {
                        state.outputs[idx].wl_global = Some(name);
                        state.outputs[idx].wl_output = Some(wl_output);
                    }
                }
            }
            OutputEvent::Removed => {
                // Migrate this output's tags to the first remaining output and
                // recompute focus. Nothing may touch `idx` afterwards.
                if let Some(idx) = idx() {
                    state.remove_output(idx);
                    data.layer_default_set = false;
                    changed = true;
                }
            }
        }
        drop(state);
        if changed {
            request_manage(data);
        }
    }
}

/// Dispatch implementation for Layer-Shell surface exclusive area updates.
impl Dispatch<RiverLayerShellOutputV1, ()> for AppData {
    fn event(
        data: &mut Self,
        layer_output: &RiverLayerShellOutputV1,
        event: LayerOutputEvent,
        _ud: &(),
        _conn: &WlConnection,
        _qh: &QueueHandle<Self>,
    ) {
        let oid = layer_output.id();
        let mut state = data.state.borrow_mut();
        let Some(output) = state
            .outputs
            .iter_mut()
            .find(|output| output.layer.as_ref().is_some_and(|layer| layer.id() == oid))
        else {
            return;
        };

        let LayerOutputEvent::NonExclusiveArea {
            x,
            y,
            width,
            height,
        } = event;
        output.usable_x = x;
        output.usable_y = y;
        output.usable_width = width.max(0) as u32;
        output.usable_height = height.max(0) as u32;
        state.repair_outputs();
        drop(state);
        request_manage(data);
    }
}

/// Dispatch implementation for input seat events (pointer entry, move, button clicks).
impl Dispatch<RiverSeatV1, ()> for AppData {
    fn event(
        data: &mut Self,
        seat: &RiverSeatV1,
        event: SeatEvent,
        _ud: &(),
        _conn: &WlConnection,
        _qh: &QueueHandle<Self>,
    ) {
        let sid = seat.id();

        if matches!(event, SeatEvent::Removed) {
            data.bindings.retain(|(binding, _, owner)| {
                if *owner == sid {
                    binding.destroy();
                    false
                } else {
                    true
                }
            });
            data.pointer_bindings.retain(|(binding, _, owner)| {
                if *owner == sid {
                    binding.destroy();
                    false
                } else {
                    true
                }
            });
            // Drop interactive ops driven by this seat: their seat proxy is
            // destroyed below, so a later manage sequence must not drive it.
            if data
                .pointer_op
                .as_ref()
                .is_some_and(|op| op.seat.id() == sid)
            {
                data.pointer_op = None;
                data.op_end_requested = false;
            }
            if data
                .pending_op
                .as_ref()
                .is_some_and(|op| op.seat.id() == sid)
            {
                data.pending_op = None;
            }
        }

        // Interactive pointer operation events (floating move/resize) need to
        // mutate the op state and window geometry, so handle them before taking
        // the general `state` borrow.
        match &event {
            SeatEvent::OpDelta { dx, dy } => {
                crate::bindings::apply_pointer_delta(data, *dx, *dy);
                return;
            }
            SeatEvent::OpRelease => {
                crate::bindings::end_pointer_op(data);
                return;
            }
            _ => {}
        }

        let mut state = data.state.borrow_mut();
        let mut output_focus_changed = false;

        // Cursor: bind this seat's wl_seat and take a wl_pointer from it. River
        // shows what the WM sets on it whenever no client has pointer focus,
        // i.e. during pointer operations. Each seat gets a fresh bind (river
        // keeps a 1:1 river_seat_v1 <-> wl_seat mapping).
        match event {
            SeatEvent::WlSeat { name } => {
                if let (Some(registry), Some(qh)) = (data.registry.clone(), data.qh.clone()) {
                    let wl_seat = registry.bind::<WlSeat, _, _>(name, 4, &qh, ());
                    let pointer = wl_seat.get_pointer(&qh, ());
                    let shape = data
                        .cursor_shape
                        .clone()
                        .map(|manager| manager.get_pointer(&pointer, &qh, ()));
                    if let Some(seat) = state.seats.iter_mut().find(|s| s.proxy.id() == sid) {
                        seat.pointer = Some(pointer);
                        seat.cursor_shape = shape;
                    }
                }
                return;
            }
            SeatEvent::PointerEnter { window } => {
                if let Some(wid) = state.find_window_by_proxy(&window) {
                    if let Some(seat) = state.seats.iter_mut().find(|s| s.proxy.id() == sid) {
                        seat.pointer_window = Some(wid);
                    }
                }
                if data.config.focus_follows_mouse {
                    if let Some(wid) = state.find_window_by_proxy(&window) {
                        if focus_window_on_output(&mut state, wid) {
                            request_manage(data);
                        }
                    }
                }
            }
            SeatEvent::PointerLeave => {
                if let Some(seat) = state.seats.iter_mut().find(|s| s.proxy.id() == sid) {
                    seat.pointer_window = None;
                }
            }
            SeatEvent::WindowInteraction { window } => {
                // A pointer button press / touch on a window: focus it,
                // regardless of focus-follows-mouse, and restore it if it was
                // minimized (river sends this even for a hidden window, e.g.
                // from a taskbar, so this is the only way back). River starts a
                // manage sequence after this event, so no extra request.
                if let Some(wid) = state.find_window_by_proxy(&window) {
                    if let Some(w) = state.find_window_mut(wid) {
                        w.minimized = false;
                    }
                    focus_window_on_output(&mut state, wid);
                }
            }
            SeatEvent::PointerPosition { x, y } => {
                if let Some(seat) = state.seats.iter_mut().find(|s| s.proxy.id() == sid) {
                    seat.pointer_x = x;
                    seat.pointer_y = y;
                }
                // Track the raw pointer and switch the focused output to the one
                // containing the pointer. This is what makes focus-follows-mouse
                // work over *empty* desktop: river only sends pointer_enter when
                // the pointer enters a window, so without this, an output with no
                // windows can never gain focus (and newly spawned windows are
                // stuck on the first output).
                if data.config.focus_follows_mouse {
                    let prev = state.focused_output;
                    let target = state.outputs.iter().position(|o| {
                        x >= o.x
                            && x < o.x + o.width as i32
                            && y >= o.y
                            && y < o.y + o.height as i32
                    });
                    if let Some(new_out) = target {
                        if prev != Some(new_out) {
                            state.focused_output = Some(new_out);
                            output_focus_changed = true;
                        }
                    }
                }
            }
            SeatEvent::Removed => {
                if let Some(seat) = state.seats.iter().find(|s| s.proxy.id() == sid) {
                    if let Some(layer) = &seat.layer {
                        layer.destroy();
                    }
                    seat.proxy.destroy();
                }
                state.seats.retain(|s| s.proxy.id() != sid);
            }
            _ => {}
        }

        drop(state);
        if output_focus_changed {
            request_manage(data);
        }
    }
}

impl Dispatch<WlOutput, u32> for AppData {
    fn event(
        data: &mut Self,
        _output: &WlOutput,
        event: WlOutputEvent,
        wl_global: &u32,
        _conn: &WlConnection,
        _qh: &QueueHandle<Self>,
    ) {
        if let WlOutputEvent::Name { name } = event {
            let mut state = data.state.borrow_mut();
            if let Some(output) = state
                .outputs
                .iter_mut()
                .find(|output| output.wl_global == Some(*wl_global))
            {
                output.name = Some(name);
            }
        }
    }
}

/// Focus `wid` and the tag/output it lives on, unless it is already there.
///
/// Goes through [`State::switch_active_tag`] rather than setting `active_tag`
/// directly: switching a tag must clear fullscreen on the tag left behind, or
/// river keeps drawing the fullscreen window over both tags.
fn focus_window_on_output(state: &mut State, wid: u32) -> bool {
    let Some(w) = state.find_window(wid) else {
        return false;
    };
    let tag = w.tag;
    let Some(output) = state.tag_owner(tag).filter(|o| *o < state.outputs.len()) else {
        return false;
    };
    if state.focused_output == Some(output)
        && state.outputs[output].active_tag == tag
        && state.outputs[output].focused_window == Some(wid)
    {
        return false;
    }
    state.focused_output = Some(output);
    state.outputs[output].focused_window = Some(wid);
    state.switch_active_tag(output, tag);
    true
}

/// Set the pointer cursor river shows while no client has pointer focus.
///
/// The seat is looked up by object id, since `State::seats` can be reordered
/// or emptied (seat removal) independently of the op that references it.
pub fn set_seat_cursor(
    state: &std::cell::RefCell<State>,
    seat: &RiverSeatV1,
    shape: wayland_protocols::wp::cursor_shape::v1::client::wp_cursor_shape_device_v1::Shape,
) {
    if let Some(seat) = state.borrow().seats.iter().find(|s| s.proxy.id() == seat.id()) {
        seat.set_cursor(shape);
    }
}

fn request_manage(data: &AppData) {
    if let Some(wm) = &data.wm {
        wm.manage_dirty();
    }
}
