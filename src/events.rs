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
                state.outputs.remove(idx);
                let outputs_empty = state.outputs.is_empty();
                for w in state.windows.iter_mut() {
                    if outputs_empty || w.output == idx {
                        w.output = 0;
                    } else if w.output > idx {
                        w.output -= 1;
                    }
                }
                // Recompute focus if the focused output was removed.
                if state.focused_output == Some(idx) {
                    state.focused_output = if state.outputs.is_empty() {
                        None
                    } else {
                        Some(0)
                    };
                } else if state.focused_output.is_some_and(|focused| focused > idx) {
                    state.focused_output = state.focused_output.map(|focused| focused - 1);
                }
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
        let mut state = data.state.borrow_mut();
        match event {
            SeatEvent::PointerEnter { window } => {
                if data.config.focus_follows_mouse {
                    if let Some(wid) = state.find_window_by_proxy(&window) {
                        // Focus switches to the window under the pointer, and
                        // thus to its output/tag.
                        if let Some((output, tag)) =
                            state.find_window(wid).map(|w| (w.output, w.tag))
                        {
                            if output < state.outputs.len() {
                                state.focused_output = Some(output);
                                state.outputs[output].focused_window = Some(wid);
                                if (state.outputs[output].active_mask >> tag) & 1 == 0 {
                                    state.outputs[output].active_mask = 1u32 << tag;
                                }
                            }
                        }
                    }
                }
            }
            SeatEvent::WindowInteraction { window } => {
                // A pointer button press / touch on a window: focus it,
                // regardless of focus-follows-mouse.
                if let Some(wid) = state.find_window_by_proxy(&window) {
                    if let Some((output, tag)) = state.find_window(wid).map(|w| (w.output, w.tag)) {
                        if output < state.outputs.len() {
                            state.focused_output = Some(output);
                            state.outputs[output].focused_window = Some(wid);
                            if (state.outputs[output].active_mask >> tag) & 1 == 0 {
                                state.outputs[output].active_mask = 1u32 << tag;
                            }
                        }
                    }
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
