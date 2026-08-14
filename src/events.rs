//! Dispatch implementations for river window/output/seat events, updating the
//! data model (titles, app ids, dimensions, output geometry, focus).

use wayland_client::{
    protocol::wl_output::{Event as WlOutputEvent, WlOutput},
    Connection as WlConnection, Dispatch, Proxy, QueueHandle,
};

use crate::connection::AppData;
use crate::protocols::layer_shell::river_layer_shell_output_v1::{
    Event as LayerOutputEvent, RiverLayerShellOutputV1,
};
use crate::protocols::wm::river_output_v1::{Event as OutputEvent, RiverOutputV1};
use crate::protocols::wm::river_seat_v1::{Event as SeatEvent, RiverSeatV1};
use crate::protocols::wm::river_window_v1::{Event as WindowEvent, RiverWindowV1};

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
        let mut state = data.state.borrow_mut();
        let Some(id) = state.find_window_by_proxy(window) else {
            return;
        };
        match event {
            WindowEvent::Closed => {
                state.windows.retain(|w| w.proxy.id() != oid);
            }
            WindowEvent::AppId { app_id } => {
                if let Some(w) = state.find_window_mut(id) {
                    // Auto-float windows whose app id is in the configured
                    // "never tile" list (password forms, calculator, Google
                    // Meet call window, ...).
                    if let Some(app_id) = app_id.as_deref() {
                        if data.config.is_floating_app(app_id) {
                            w.floating = true;
                        }
                    }
                    w.app_id = app_id;
                }
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
            _ => {}
        }
    }
}

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
        let Some(idx) = state.outputs.iter().position(|o| o.proxy.id() == oid) else {
            return;
        };
        match event {
            OutputEvent::Position { x, y } => {
                state.outputs[idx].x = x;
                state.outputs[idx].y = y;
                if state.outputs[idx].usable_width == 0 && state.outputs[idx].usable_height == 0 {
                    state.outputs[idx].usable_x = x;
                    state.outputs[idx].usable_y = y;
                }
            }
            OutputEvent::Dimensions { width, height } => {
                state.outputs[idx].width = width as u32;
                state.outputs[idx].height = height as u32;
                if state.outputs[idx].usable_width == 0 && state.outputs[idx].usable_height == 0 {
                    state.outputs[idx].usable_width = width as u32;
                    state.outputs[idx].usable_height = height as u32;
                }
            }
            OutputEvent::WlOutput { name } => {
                state.outputs[idx].wl_global = Some(name);
                if let (Some(registry), Some(qh)) = (data.registry.clone(), data.qh.clone()) {
                    let wl_output = registry.bind::<WlOutput, _, _>(name, 4, &qh, name);
                    state.outputs[idx].wl_output = Some(wl_output);
                }
            }
            OutputEvent::Removed => {
                // Migrate this output's tags to the first remaining output and
                // recompute focus.
                state.remove_output(idx);
            }
        }
    }
}

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
    }
}

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
        match event {
            SeatEvent::PointerEnter { window } => {
                if let Some(wid) = state.find_window_by_proxy(&window) {
                    if let Some(seat) = state.seats.iter_mut().find(|s| s.proxy.id() == sid) {
                        seat.pointer_window = Some(wid);
                    }
                }
                if data.config.focus_follows_mouse {
                    if let Some(wid) = state.find_window_by_proxy(&window) {
                        // Focus switches to the window under the pointer, and
                        // thus to its tag's output.
                        if let Some(tag) = state.find_window(wid).map(|w| w.tag) {
                            if let Some(output) = state.tag_owner(tag) {
                                if output < state.outputs.len() {
                                    state.focused_output = Some(output);
                                    state.outputs[output].focused_window = Some(wid);
                                    state.outputs[output].active_tag = tag;
                                }
                            }
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
                // regardless of focus-follows-mouse.
                if let Some(wid) = state.find_window_by_proxy(&window) {
                    if let Some(tag) = state.find_window(wid).map(|w| w.tag) {
                        if let Some(output) = state.tag_owner(tag) {
                            if output < state.outputs.len() {
                                state.focused_output = Some(output);
                                state.outputs[output].focused_window = Some(wid);
                                state.outputs[output].active_tag = tag;
                            }
                        }
                    }
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
                    state.focused_output = state
                        .outputs
                        .iter()
                        .position(|o| {
                            x >= o.x
                                && x < o.x + o.width as i32
                                && y >= o.y
                                && y < o.y + o.height as i32
                        })
                        .or(state.focused_output);
                }
            }
            SeatEvent::Removed => {
                state.seats.retain(|s| s.proxy.id() != sid);
            }
            _ => {}
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
