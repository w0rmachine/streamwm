//! Core state model: outputs, seats, windows, and the unified global tag table.

use std::sync::atomic::{AtomicU32, Ordering};

use wayland_client::protocol::wl_output::WlOutput;
use wayland_client::Proxy;

use crate::protocols::wm::{river_seat_v1::RiverSeatV1, river_window_v1::RiverWindowV1};

/// Global monotonically-increasing id allocator for streamwm window ids
/// (used in our status protocol; distinct from river object ids).
static NEXT_WINDOW_ID: AtomicU32 = AtomicU32::new(1);

/// Number of global tags (0..=8, displayed to users as 1..=9).
pub const NUM_TAGS: usize = 9;

/// A global tag. Tags are shared across all outputs: each tag is owned by at
/// most one output (`output`), and a window lives on exactly one tag.
pub struct Tag {
    /// The output index that currently owns this tag (`None` = unassigned).
    pub output: Option<usize>,
    /// Per-tag label (None = default numeric label).
    pub label: Option<String>,
}

impl Tag {
    fn unassigned() -> Tag {
        Tag {
            output: None,
            label: None,
        }
    }
}

/// A logical window under management.
pub struct Window {
    pub proxy: RiverWindowV1,
    /// streamwm id exposed to the status protocol.
    pub id: u32,
    /// Global tag id this window belongs to (0..=NUM_TAGS-1).
    pub tag: usize,
    pub app_id: Option<String>,
    pub title: Option<String>,
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
    /// Whether the window should be fullscreen (desired state).
    pub fullscreen: bool,
    /// Whether the fullscreen request matching `fullscreen` has been sent.
    pub fullscreen_applied: bool,
    /// Last decoration mode sent to river (`true` = SSD, `false` = CSD).
    pub ssd_applied: Option<bool>,
    /// Render node (obtained once via get_node).
    pub node: Option<crate::protocols::wm::river_node_v1::RiverNodeV1>,
}

impl Window {
    pub fn new(proxy: RiverWindowV1, tag: usize) -> Window {
        Window {
            id: NEXT_WINDOW_ID.fetch_add(1, Ordering::Relaxed),
            proxy,
            tag,
            app_id: None,
            title: None,
            floating: false,
            float_x: 0,
            float_y: 0,
            float_w: 400,
            float_h: 300,
            width: 0,
            height: 0,
            fullscreen: false,
            fullscreen_applied: false,
            ssd_applied: None,
            node: None,
        }
    }
}

/// A physical output (monitor).
pub struct Output {
    pub proxy: crate::protocols::wm::river_output_v1::RiverOutputV1,
    /// Output name (e.g. eDP-1), populated once wl_output is resolved.
    pub name: Option<String>,
    /// wl_output global name announced by river_output_v1.wl_output.
    pub wl_global: Option<u32>,
    /// Bound wl_output proxy kept alive so WlOutput::name arrives.
    pub wl_output: Option<WlOutput>,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    /// Area left after layer-shell exclusive zones, in global coordinates.
    pub usable_x: i32,
    pub usable_y: i32,
    pub usable_width: u32,
    pub usable_height: u32,
    /// The tag currently shown (active) on this output.
    pub active_tag: usize,
    /// Focused window id (streamwm id) on this output, if any.
    pub focused_window: Option<u32>,
    /// Layer-shell output state (created once via get_output).
    pub layer:
        Option<crate::protocols::layer_shell::river_layer_shell_output_v1::RiverLayerShellOutputV1>,
}

impl Output {
    pub fn new(proxy: crate::protocols::wm::river_output_v1::RiverOutputV1) -> Output {
        Output {
            proxy,
            name: None,
            wl_global: None,
            wl_output: None,
            x: 0,
            y: 0,
            width: 0,
            height: 0,
            usable_x: 0,
            usable_y: 0,
            usable_width: 0,
            usable_height: 0,
            active_tag: 0,
            focused_window: None,
            layer: None,
        }
    }
}

/// A seat (input device group).
pub struct Seat {
    pub proxy: RiverSeatV1,
    pub pointer_x: i32,
    pub pointer_y: i32,
    /// Window currently under the pointer (streamwm id), if any.
    pub pointer_window: Option<u32>,
    /// Layer-shell seat state (created once via get_seat).
    pub layer:
        Option<crate::protocols::layer_shell::river_layer_shell_seat_v1::RiverLayerShellSeatV1>,
}

impl Seat {
    pub fn new(proxy: RiverSeatV1) -> Seat {
        Seat {
            proxy,
            pointer_x: 0,
            pointer_y: 0,
            pointer_window: None,
            layer: None,
        }
    }
}

/// Reassign tag ownership after an output is removed.
///
/// `owners[t]` is the output index owning tag `t` (`None` = unassigned),
/// `removed` is the index of the removed output, and `remaining` is the number
/// of outputs left after removal. Tags owned by the removed output migrate to
/// the first remaining output (index 0), and owners above `removed` shift down.
fn remap_tag_owners(owners: &mut [Option<usize>], removed: usize, remaining: usize) {
    for owner in owners.iter_mut() {
        match *owner {
            None => {}
            Some(_) if remaining == 0 => *owner = None,
            Some(o) if o == removed => *owner = Some(0),
            Some(o) if o > removed => *owner = Some(o - 1),
            Some(_) => {}
        }
    }
}

/// The aggregate window manager state.
pub struct State {
    pub outputs: Vec<Output>,
    pub seats: Vec<Seat>,
    /// streamwm id -> window.
    pub windows: Vec<Window>,
    /// Global tags (shared across outputs).
    pub tags: Vec<Tag>,
    /// The output index that currently has seat focus.
    pub focused_output: Option<usize>,
    /// Live master fraction for the tiling layout (adjustable in resize mode).
    pub master_fraction: f64,
    /// Whether the WM is in resize mode (arrow keys resize the layout).
    pub resize_mode: bool,
}

impl State {
    pub fn new() -> State {
        State {
            outputs: Vec::new(),
            seats: Vec::new(),
            windows: Vec::new(),
            tags: (0..NUM_TAGS).map(|_| Tag::unassigned()).collect(),
            focused_output: None,
            master_fraction: 0.55,
            resize_mode: false,
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
        self.outputs
            .iter()
            .position(|o| o.name.as_deref() == Some(name))
    }

    /// The focused output index, or the first output (0) as fallback.
    pub fn active_output(&self) -> Option<usize> {
        self.focused_output.or(if self.outputs.is_empty() {
            None
        } else {
            Some(0)
        })
    }

    /// The output owning a tag, if any.
    pub fn tag_owner(&self, tag: usize) -> Option<usize> {
        self.tags.get(tag).and_then(|t| t.output)
    }

    /// The output a window is displayed on (derived from its tag's owner).
    pub fn window_output(&self, window: &Window) -> Option<usize> {
        self.tag_owner(window.tag)
    }

    /// Whether a window is currently visible (its tag is active on its owner).
    pub fn window_is_visible(&self, window: &Window) -> bool {
        self.tag_owner(window.tag)
            .and_then(|o| self.outputs.get(o))
            .map(|o| o.active_tag == window.tag)
            .unwrap_or(false)
    }

    /// Assign the first unassigned tag to an output (used on output creation).
    pub fn assign_initial_tag(&mut self, output_idx: usize) {
        if self.tags.iter().any(|t| t.output == Some(output_idx)) {
            return;
        }
        let tag = self.tags.iter().position(|t| t.output.is_none());
        if let Some(tag) = tag {
            self.tags[tag].output = Some(output_idx);
            if let Some(o) = self.outputs.get_mut(output_idx) {
                o.active_tag = tag;
            }
        }
    }

    /// Remove an output and migrate its tags to the first remaining output.
    pub fn remove_output(&mut self, idx: usize) {
        if idx >= self.outputs.len() {
            return;
        }
        self.outputs.remove(idx);
        let remaining = self.outputs.len();
        let mut owners: Vec<Option<usize>> = self.tags.iter().map(|t| t.output).collect();
        remap_tag_owners(&mut owners, idx, remaining);
        for (tag, owner) in self.tags.iter_mut().zip(owners) {
            tag.output = owner;
        }

        if self.focused_output == Some(idx) {
            self.focused_output = if remaining == 0 { None } else { Some(0) };
        } else if self.focused_output.is_some_and(|f| f > idx) {
            self.focused_output = self.focused_output.map(|f| f - 1);
        }
    }

    /// Focus a global tag from an output.
    ///
    /// * Unassigned tag -> assign it to `output_idx` and activate it there.
    /// * Tag owned by `output_idx` -> activate it there.
    /// * Tag owned by another output -> switch focus to that output and show
    ///   the tag there (river-style global navigation).
    pub fn focus_tag(&mut self, output_idx: usize, tag: usize) {
        if tag >= NUM_TAGS || output_idx >= self.outputs.len() {
            return;
        }
        let owner = self.tags.get(tag).and_then(|t| t.output);
        match owner {
            None => {
                self.tags[tag].output = Some(output_idx);
                if let Some(o) = self.outputs.get_mut(output_idx) {
                    o.active_tag = tag;
                }
                self.refocus_output(output_idx);
            }
            Some(owner) if owner == output_idx => {
                if let Some(o) = self.outputs.get_mut(output_idx) {
                    o.active_tag = tag;
                }
                self.refocus_output(output_idx);
            }
            Some(owner) => {
                self.focused_output = Some(owner);
                if let Some(o) = self.outputs.get_mut(owner) {
                    o.active_tag = tag;
                }
                self.refocus_output(owner);
            }
        }
    }

    /// Move the focused window of an output to a global tag. Unassigned tags
    /// are created (assigned) on the output first.
    pub fn send_focused_to_tag(&mut self, output_idx: usize, tag: usize) {
        if tag >= NUM_TAGS || output_idx >= self.outputs.len() {
            return;
        }
        if self.tags.get(tag).and_then(|t| t.output).is_none() {
            self.tags[tag].output = Some(output_idx);
        }
        let focused = self.outputs[output_idx].focused_window;
        if let Some(fid) = focused {
            if let Some(w) = self.find_window_mut(fid) {
                w.tag = tag;
            }
        }
        self.refocus_output(output_idx);
    }

    /// Recompute the focused window for an output.
    pub fn refocus_output(&mut self, output_idx: usize) {
        let active = self.outputs[output_idx].active_tag;
        let focused = self.outputs[output_idx].focused_window;
        // Keep focus if the focused window is still on the active tag of this
        // output.
        let keep = focused
            .and_then(|fid| self.find_window(fid))
            .map(|w| self.tag_owner(w.tag) == Some(output_idx) && w.tag == active)
            .unwrap_or(false);

        let new_focused = if keep {
            focused
        } else {
            // Pick the first window on the active tag of this output.
            self.windows
                .iter()
                .find(|w| self.tag_owner(w.tag) == Some(output_idx) && w.tag == active)
                .map(|w| w.id)
        };

        if let Some(o) = self.outputs.get_mut(output_idx) {
            o.focused_window = new_focused;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remap_keeps_lower_owners_and_shifts_upper() {
        // outputs: [0, 1, 2], remove index 1.
        let mut owners = [Some(0), Some(1), Some(2), None, Some(3)];
        remap_tag_owners(&mut owners, 1, 2);
        assert_eq!(owners, [Some(0), Some(0), Some(1), None, Some(2)]);
    }

    #[test]
    fn remap_moves_removed_output_tags_to_first_output() {
        // remove index 0 -> its tags move to output 0 (which was output 1).
        let mut owners = [Some(0), Some(1)];
        remap_tag_owners(&mut owners, 0, 1);
        assert_eq!(owners, [Some(0), Some(0)]);
    }

    #[test]
    fn remap_unassigns_all_when_no_outputs_remain() {
        let mut owners = [Some(0), Some(1), None];
        remap_tag_owners(&mut owners, 0, 0);
        assert_eq!(owners, [None, None, None]);
    }
}
