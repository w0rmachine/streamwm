//! Core state model: outputs, seats, windows, and the per-output tag table.

use std::sync::atomic::{AtomicU32, Ordering};

use wayland_client::Proxy;

use crate::protocols::wm::{
    river_seat_v1::RiverSeatV1,
    river_window_v1::RiverWindowV1,
};

/// Global monotonically-increasing id allocator for streamwm window ids
/// (used in our status protocol; distinct from river object ids).
static NEXT_WINDOW_ID: AtomicU32 = AtomicU32::new(1);

/// Number of tags per output (0..=9).
pub const NUM_TAGS: usize = 10;

/// A logical window under management.
pub struct Window {
    pub proxy: RiverWindowV1,
    /// streamwm id exposed to the status protocol.
    pub id: u32,
    pub app_id: Option<String>,
    pub title: Option<String>,
    /// Tag id this window currently belongs to (0..=9).
    pub tag: usize,
    /// Whether the window is floating.
    pub floating: bool,
    /// Floating position/size (logical px, global coords).
    pub float_x: i32,
    pub float_y: i32,
    pub float_w: u32,
    pub float_h: u32,
    /// Last known content dimensions (from river_window_v1.dimensions).
    pub width: u32,
    pub height: u32,
    /// Whether the window is fullscreen.
    pub fullscreen: bool,
    /// Render node (obtained once via get_node).
    pub node: Option<crate::protocols::wm::river_node_v1::RiverNodeV1>,
}

impl Window {
    pub fn new(proxy: RiverWindowV1) -> Window {
        Window {
            id: NEXT_WINDOW_ID.fetch_add(1, Ordering::Relaxed),
            proxy,
            app_id: None,
            title: None,
            tag: 0,
            floating: false,
            float_x: 0,
            float_y: 0,
            float_w: 400,
            float_h: 300,
            width: 0,
            height: 0,
            fullscreen: false,
            node: None,
        }
    }
}

/// A physical output (monitor).
pub struct Output {
    pub proxy: crate::protocols::wm::river_output_v1::RiverOutputV1,
    /// Output name (e.g. eDP-1), populated once wl_output is resolved.
    pub name: Option<String>,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    /// Bitmask of active (visible) tag ids.
    pub active_mask: u32,
    /// Bitmask of occupied (non-empty) tag ids.
    pub occupied_mask: u32,
    /// Bitmask of urgent tag ids.
    pub urgent_mask: u32,
    /// Per-tag labels (None = default numeric label).
    pub tag_labels: Vec<Option<String>>,
    /// Focused window id (streamwm id) on this output, if any.
    pub focused_window: Option<u32>,
}

impl Output {
    pub fn new(
        proxy: crate::protocols::wm::river_output_v1::RiverOutputV1,
    ) -> Output {
        Output {
            proxy,
            name: None,
            x: 0,
            y: 0,
            width: 0,
            height: 0,
            active_mask: 1, // tag 0 active by default
            occupied_mask: 0,
            urgent_mask: 0,
            tag_labels: vec![None; NUM_TAGS],
            focused_window: None,
        }
    }

    pub fn active_tags(&self) -> Vec<usize> {
        (0..NUM_TAGS)
            .filter(|t| (self.active_mask >> t) & 1 == 1)
            .collect()
    }
}

/// A seat (input device group).
pub struct Seat {
    pub proxy: RiverSeatV1,
    pub pointer_x: i32,
    pub pointer_y: i32,
    /// Window currently under the pointer (streamwm id), if any.
    pub pointer_window: Option<u32>,
}

impl Seat {
    pub fn new(proxy: RiverSeatV1) -> Seat {
        Seat {
            proxy,
            pointer_x: 0,
            pointer_y: 0,
            pointer_window: None,
        }
    }
}

/// The aggregate window manager state.
pub struct State {
    pub outputs: Vec<Output>,
    pub seats: Vec<Seat>,
    /// streamwm id -> window.
    pub windows: Vec<Window>,
    /// The output index that currently has seat focus.
    pub focused_output: Option<usize>,
}

impl State {
    pub fn new() -> State {
        State {
            outputs: Vec::new(),
            seats: Vec::new(),
            windows: Vec::new(),
            focused_output: None,
        }
    }

    pub fn find_window(&self, id: u32) -> Option<&Window> {
        self.windows.iter().find(|w| w.id == id)
    }

    pub fn find_window_mut(&mut self, id: u32) -> Option<&mut Window> {
        self.windows.iter_mut().find(|w| w.id == id)
    }

    pub fn find_window_by_proxy(&self, proxy: &RiverWindowV1) -> Option<u32> {
        self.windows
            .iter()
            .find(|w| w.proxy.id() == proxy.id())
            .map(|w| w.id)
    }

    /// Output index by name.
    pub fn find_output_by_name(&self, name: &str) -> Option<usize> {
        self.outputs.iter().position(|o| o.name.as_deref() == Some(name))
    }

    /// The focused output index, or the first output (0) as fallback.
    pub fn active_output(&self) -> Option<usize> {
        self.focused_output.or_else(|| {
            if self.outputs.is_empty() {
                None
            } else {
                Some(0)
            }
        })
    }

    /// Set the active tag mask of an output to a single tag.
    pub fn focus_tag(&mut self, output_idx: usize, tag: usize) {
        if let Some(o) = self.outputs.get_mut(output_idx) {
            o.active_mask = 1u32 << tag;
            // Move focus to the topmost window of that tag.
            self.refocus_output(output_idx);
        }
    }

    /// Move the focused window of an output to a tag.
    pub fn send_focused_to_tag(&mut self, output_idx: usize, tag: usize) {
        let focused = self.outputs[output_idx].focused_window;
        if let Some(fid) = focused {
            if let Some(w) = self.find_window_mut(fid) {
                w.tag = tag;
            }
        }
    }

    /// Recompute focus + occupied masks and the focused window for an output.
    pub fn refocus_output(&mut self, output_idx: usize) {
        // Recompute occupancy.
        let mut occupied = 0u32;
        for w in self.windows.iter() {
            occupied |= 1u32 << w.tag;
        }
        let active = {
            let o = &self.outputs[output_idx];
            if o.active_mask == 0 {
                // never leave no tags active
                1
            } else {
                o.active_mask
            }
        };
        let focused = {
            let o = &self.outputs[output_idx];
            o.focused_window
        };
        // Keep focus if the focused window is still on an active tag.
        let keep = focused
            .and_then(|fid| self.find_window(fid))
            .map(|w| (active >> w.tag) & 1 == 1)
            .unwrap_or(false);

        let new_focused = if keep {
            focused
        } else {
            // Pick the first window on an active tag.
            self.windows
                .iter()
                .find(|w| (active >> w.tag) & 1 == 1)
                .map(|w| w.id)
        };

        if let Some(o) = self.outputs.get_mut(output_idx) {
            o.occupied_mask = occupied;
            o.active_mask = active;
            o.focused_window = new_focused;
        }
    }
}
