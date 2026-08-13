//! Dispatch implementations for river window/output/seat events, updating the
//! data model (titles, app ids, dimensions, output geometry, focus).

use wayland_client::{Connection as WlConnection, Dispatch, Proxy, QueueHandle};

use crate::connection::AppData;
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
            WindowEvent::Closed {} => {
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
            }
            OutputEvent::Dimensions { width, height } => {
                state.outputs[idx].width = width as u32;
                state.outputs[idx].height = height as u32;
            }
            OutputEvent::Removed {} => {
                state.outputs.remove(idx);
                // Recompute focus if the focused output was removed.
                if state.focused_output == Some(idx) {
                    state.focused_output = if state.outputs.is_empty() {
                        None
                    } else {
                        Some(0)
                    };
                }
            }
            _ => {}
        }
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
                        if let Some(tag) = state.find_window(wid).map(|w| w.tag) {
                            if let Some(o) = state.active_output() {
                                state.outputs[o].focused_window = Some(wid);
                                // If the window is on another tag, activate it.
                                state.outputs[o].active_mask |= 1u32 << tag;
                            }
                        }
                    }
                }
            }
            SeatEvent::Removed {} => {
                state.seats.retain(|s| s.proxy.id() != sid);
            }
            _ => {}
        }
    }
}
