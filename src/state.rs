//! Core data model: manages outputs, seats, windows, global tag table, and focus state.
//!
//! Includes workspace calculations, multi-monitor tag owner remapping, active tag selection,
//! and floating window coordinate boundary clamping.

use std::sync::atomic::{AtomicU32, Ordering};

use wayland_client::protocol::wl_output::WlOutput;
use wayland_client::Proxy;

use crate::protocols::wm::{river_seat_v1::RiverSeatV1, river_window_v1::RiverWindowV1};

/// Monotonically-increasing window ID generator for streamwm internal protocol tracking.
static NEXT_WINDOW_ID: AtomicU32 = AtomicU32::new(1);

/// Fixed total count of global tags (0..=8, exposed to users as tags 1..=9).
pub const NUM_TAGS: usize = 9;

/// A global tag structure. Tags are shared globally across all outputs.
/// Each tag is owned by at most one output (`output`), and a window belongs to exactly one tag.
pub struct Tag {
    /// The index of the physical output currently owning this tag (`None` = unassigned).
    pub output: Option<usize>,
    /// Optional display label for status bars.
    pub label: Option<String>,
    /// Master fraction of this tag's tiling layout (0.1..=0.9), preserved per tag.
    pub master_fraction: f64,
}

impl Tag {
    /// Create a new unassigned tag initialized with the default master layout fraction.
    fn unassigned(master_fraction: f64) -> Tag {
        Tag {
            output: None,
            label: None,
            master_fraction,
        }
    }
}

/// A logical managed client window.
pub struct Window {
    /// Wayland River window protocol proxy.
    pub proxy: RiverWindowV1,
    /// Internal streamwm window identifier exposed over the status socket interface.
    pub id: u32,
    /// Global tag index this window is attached to (0..=NUM_TAGS-1).
    pub tag: usize,
    /// Streamwm id of this window's parent/transient (dialog), if any.
    pub parent: Option<u32>,
    /// Client application ID (e.g. `"alacritty"`, `"firefox"`).
    pub app_id: Option<String>,
    /// Current window title text.
    pub title: Option<String>,
    /// Whether the window is floating (un-tiled).
    pub floating: bool,
    /// Floating X coordinate in global layout space (logical pixels).
    pub float_x: i32,
    /// Floating Y coordinate in global layout space (logical pixels).
    pub float_y: i32,
    /// Floating width (logical pixels).
    pub float_w: u32,
    /// Floating height (logical pixels).
    pub float_h: u32,
    /// Last reported content width from `RiverWindowV1.dimensions`.
    pub width: u32,
    /// Last reported content height from `RiverWindowV1.dimensions`.
    pub height: u32,
    /// Preferred min/max dimensions from `dimensions_hint` (0 = no preference).
    pub min_width: u32,
    pub min_height: u32,
    pub max_width: u32,
    pub max_height: u32,
    /// Desired fullscreen state flag.
    pub fullscreen: bool,
    /// True if fullscreen request has been sent to River.
    pub fullscreen_applied: bool,
    /// Whether the window is minimized (hidden) by a window-initiated request.
    pub minimized: bool,
    /// Last applied decoration mode (`Some(true)` = SSD, `Some(false)` = CSD).
    pub ssd_applied: Option<bool>,
    /// Whether `set_capabilities` has been sent for this window yet.
    pub caps_set: bool,
    /// Last `set_tiled` state sent (Some(true) = tiled, Some(false) = not).
    pub tiled_applied: Option<bool>,
    /// Last border configuration sent to River: (focused, width, r, g, b).
    pub border_applied: Option<(bool, u32, u8, u8, u8)>,
    /// River render node proxy created via `get_node`.
    pub node: Option<crate::protocols::wm::river_node_v1::RiverNodeV1>,
}

impl Window {
    /// Create a new managed window instance for a protocol proxy and tag.
    pub fn new(proxy: RiverWindowV1, tag: usize) -> Window {
        Window {
            id: NEXT_WINDOW_ID.fetch_add(1, Ordering::Relaxed),
            proxy,
            tag,
            parent: None,
            app_id: None,
            title: None,
            floating: false,
            float_x: 0,
            float_y: 0,
            float_w: 400,
            float_h: 300,
            width: 0,
            height: 0,
            min_width: 0,
            min_height: 0,
            max_width: 0,
            max_height: 0,
            fullscreen: false,
            fullscreen_applied: false,
            minimized: false,
            ssd_applied: None,
            caps_set: false,
            tiled_applied: None,
            border_applied: None,
            node: None,
        }
    }
}

/// A physical display output (monitor).
pub struct Output {
    /// Wayland River output proxy.
    pub proxy: crate::protocols::wm::river_output_v1::RiverOutputV1,
    /// Output connector name (e.g. `"eDP-1"`, `"DP-1"`).
    pub name: Option<String>,
    /// Wayland global registry ID for `wl_output`.
    pub wl_global: Option<u32>,
    /// Bound `wl_output` proxy handle.
    pub wl_output: Option<WlOutput>,
    /// Global X origin coordinate (logical pixels).
    pub x: i32,
    /// Global Y origin coordinate (logical pixels).
    pub y: i32,
    /// Total output width (logical pixels).
    pub width: u32,
    /// Total output height (logical pixels).
    pub height: u32,
    /// Usable X area left after layer-shell panels (e.g. Waybar).
    ///
    /// `RiverLayerShellOutputV1::non_exclusive_area` is the only writer of the
    /// four `usable_*` fields; while they are all zero river has not reported
    /// an area yet and consumers must fall back to the full output box.
    pub usable_x: i32,
    /// Usable Y area left after layer-shell panels.
    pub usable_y: i32,
    /// Usable width left after layer-shell panels.
    pub usable_width: u32,
    /// Usable height left after layer-shell panels.
    pub usable_height: u32,
    /// Global tag index currently active (displayed) on this output.
    pub active_tag: usize,
    /// Streamwm window ID focused on this output.
    pub focused_window: Option<u32>,
    /// Bound River layer shell output state.
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
    /// `wl_pointer` of the seat's `wl_seat`: river shows the cursor the WM
    /// sets here whenever no client has pointer focus (during pointer ops).
    pub pointer: Option<wayland_client::protocol::wl_pointer::WlPointer>,
    /// `wp_cursor_shape_v1` device for that pointer, when the compositor
    /// supports cursor-shape (no buffers or theme needed).
    pub cursor_shape: Option<
        wayland_protocols::wp::cursor_shape::v1::client::wp_cursor_shape_device_v1::WpCursorShapeDeviceV1,
    >,
}

impl Seat {
    pub fn new(proxy: RiverSeatV1) -> Seat {
        Seat {
            proxy,
            pointer_x: 0,
            pointer_y: 0,
            pointer_window: None,
            layer: None,
            pointer: None,
            cursor_shape: None,
        }
    }

    /// Show `shape` (e.g. move, resize) as the WM's cursor, falling back to
    /// the compositor's default when the compositor has no cursor-shape.
    pub fn set_cursor(
        &self,
        shape: wayland_protocols::wp::cursor_shape::v1::client::wp_cursor_shape_device_v1::Shape,
    ) {
        let Some(pointer) = &self.pointer else {
            return;
        };
        match &self.cursor_shape {
            Some(device) => device.set_shape(0, shape),
            // A null cursor surface makes the compositor use its default.
            None => pointer.set_cursor(0, None, 0, 0),
        }
    }
}

impl Drop for Seat {
    fn drop(&mut self) {
        if let Some(device) = &self.cursor_shape {
            device.destroy();
        }
        if let Some(pointer) = &self.pointer {
            pointer.release();
        }
    }
}

/// Reassign tag ownership after an output is removed.
///
/// `owners[t]` is the output index owning tag `t` (`None` = unassigned),
/// `removed` is the index of the removed output, and `remaining` is the number
/// of outputs left after removal. Tags owned by the removed output migrate to
/// the first remaining output (index 0), and owners above `removed` shift down.
pub(crate) fn remap_tag_owners(owners: &mut [Option<usize>], removed: usize, remaining: usize) {
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
    /// Default master fraction applied to newly created tags.
    pub default_master_fraction: f64,
    /// Whether the WM is in resize mode (arrow keys resize the layout).
    pub resize_mode: bool,
    /// Set by any state change the compositor must apply; consumed by the
    /// event loop, which then requests a manage sequence.
    pub pending_apply: bool,
}

impl State {
    pub fn new(default_master_fraction: f64) -> State {
        State {
            outputs: Vec::new(),
            seats: Vec::new(),
            windows: Vec::new(),
            tags: (0..NUM_TAGS)
                .map(|_| Tag::unassigned(default_master_fraction))
                .collect(),
            focused_output: None,
            default_master_fraction,
            resize_mode: false,
            pending_apply: false,
        }
    }

    /// Mark the state as needing a manage sequence so river applies it.
    /// State changes reach the compositor only through a manage pass, so every
    /// mutation that the user should see must set this.
    pub fn mark_dirty(&mut self) {
        self.pending_apply = true;
    }

    /// Toggle modal resize mode, returning the new state. The key that enters
    /// resize mode also leaves it; otherwise the only exit would be a bare
    /// Escape, which resize mode itself swallows.
    pub fn toggle_resize_mode(&mut self) -> bool {
        self.resize_mode = !self.resize_mode;
        self.resize_mode
    }

    /// Master fraction for `tag`'s layout (per-tag; falls back to the default).
    pub fn master_fraction(&self, tag: usize) -> f64 {
        self.tags
            .get(tag)
            .map(|t| t.master_fraction)
            .unwrap_or(self.default_master_fraction)
    }

    /// Set the master fraction for `tag`'s layout, clamped to the legal range.
    pub fn set_master_fraction(&mut self, tag: usize, fraction: f64) {
        if let Some(t) = self.tags.get_mut(tag) {
            t.master_fraction = fraction.clamp(0.1, 0.9);
        }
    }

    /// Master fraction for the active tag of `output_idx`.
    pub fn active_master_fraction(&self, output_idx: usize) -> f64 {
        self.outputs
            .get(output_idx)
            .map(|o| self.master_fraction(o.active_tag))
            .unwrap_or(self.default_master_fraction)
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
    /// A window created before its tag is owned falls back to the focused
    /// output, which is the output it is managed against in a manage pass.
    pub fn window_output(&self, window: &Window) -> Option<usize> {
        self.tag_owner(window.tag).or(self.active_output())
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
        self.normalize_outputs();
    }

    /// Remove an output and migrate its tags to the first remaining output.
    pub fn remove_output(&mut self, idx: usize) {
        if idx >= self.outputs.len() {
            return;
        }
        let removed = self.outputs.remove(idx);
        if let Some(layer) = &removed.layer {
            layer.destroy();
        }
        if let Some(wl_output) = &removed.wl_output {
            wl_output.release();
        }
        removed.proxy.destroy();
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
        self.normalize_outputs();
        self.clamp_floating_windows();
    }

    /// Revalidate active tags, focus, and floating positions after output
    /// geometry or ownership changes.
    pub fn repair_outputs(&mut self) {
        self.normalize_outputs();
        self.clamp_floating_windows();
    }

    fn normalize_outputs(&mut self) {
        if self.outputs.is_empty() {
            self.focused_output = None;
            return;
        }

        if self
            .focused_output
            .is_none_or(|focused| focused >= self.outputs.len())
        {
            self.focused_output = Some(0);
        }

        for output_idx in 0..self.outputs.len() {
            if self.tag_owner(self.outputs[output_idx].active_tag) != Some(output_idx) {
                if let Some(tag) = self.best_tag_for_output(output_idx) {
                    self.outputs[output_idx].active_tag = tag;
                } else if let Some(tag) = self.tags.iter().position(|t| t.output.is_none()) {
                    self.tags[tag].output = Some(output_idx);
                    self.outputs[output_idx].active_tag = tag;
                }
            }
            self.refocus_output(output_idx);
        }
    }

    fn best_tag_for_output(&self, output_idx: usize) -> Option<usize> {
        let output = self.outputs.get(output_idx)?;
        let current = output.active_tag;
        if self.tag_owner(current) == Some(output_idx) {
            return Some(current);
        }

        // Compute tag occupancy bitmask in a single pass without heap allocation.
        let occupied_mask: u32 = self.windows.iter().fold(0u32, |acc, w| acc | (1u32 << w.tag));

        // Prefer an owned tag that contains windows.
        if let Some((tag, _)) = self.tags.iter().enumerate().find(|(tag, t)| {
            t.output == Some(output_idx) && ((occupied_mask & (1u32 << tag)) != 0)
        }) {
            return Some(tag);
        }

        // Fallback to any owned tag.
        self.tags.iter().position(|t| t.output == Some(output_idx))
    }

    pub fn clamp_floating_windows(&mut self) {
        if self.outputs.is_empty() {
            return;
        }

        let output_areas: Vec<(i32, i32, u32, u32)> =
            self.outputs.iter().map(output_area).collect();
        let focused_output = self.focused_output;
        let owners: Vec<Option<usize>> = self.tags.iter().map(|t| t.output).collect();

        for window in self.windows.iter_mut() {
            if !window.floating {
                continue;
            }
            // Clamp to the output the window is *shown* on: a window on a tag
            // with no owner is drawn on the active output, not on the owner's.
            let output_idx = owners
                .get(window.tag)
                .copied()
                .flatten()
                .or(focused_output)
                .unwrap_or(0);
            let Some((x, y, width, height)) = output_areas.get(output_idx).copied() else {
                continue;
            };
            if width == 0 || height == 0 {
                continue;
            }
            let (fx, fy) = clamp_rect_to_area(
                window.float_x,
                window.float_y,
                window.float_w,
                window.float_h,
                x,
                y,
                width,
                height,
            );
            window.float_x = fx;
            window.float_y = fy;
        }
    }

    /// Focus a global tag from an output.
    ///
    /// * Unassigned tag -> assign it to `output_idx` and activate it there.
    /// * Tag owned by `output_idx` -> activate it there.
    /// * Tag owned by another output -> switch focus to that output and show
    ///   the tag there (river-style global navigation).
    pub fn focus_tag(&mut self, output_idx: usize, tag: usize) {
        if tag >= self.tags.len() || output_idx >= self.outputs.len() {
            return;
        }
        let owner = self.tags.get(tag).and_then(|t| t.output);
        match owner {
            None => {
                self.tags[tag].output = Some(output_idx);
                self.activate_tag(output_idx, tag);
            }
            Some(owner) if owner == output_idx => {
                self.activate_tag(output_idx, tag);
            }
            Some(owner) => {
                self.focused_output = Some(owner);
                self.activate_tag(owner, tag);
            }
        }
        self.mark_dirty();
    }

    /// Make `tag` the active tag of `output_idx`. The tag left behind keeps
    /// its output even when empty: dropping it leaves the output tagless, and
    /// `normalize_outputs` then gives it the lowest free tag index instead.
    fn activate_tag(&mut self, output_idx: usize, tag: usize) {
        let old = self.outputs[output_idx].active_tag;
        self.outputs[output_idx].active_tag = tag;
        self.clear_fullscreen_on_tag(old);
        self.refocus_output(output_idx);
    }

    /// Make `tag` the active tag of `output_idx`, clearing fullscreen on any
    /// windows left on the previously active tag (river keeps a fullscreen
    /// window drawn over everything, which would merge two tags' windows).
    /// Used by pointer-driven focus, which bypasses [`activate_tag`].
    pub fn switch_active_tag(&mut self, output_idx: usize, tag: usize) {
        if output_idx >= self.outputs.len() {
            return;
        }
        let old = self.outputs[output_idx].active_tag;
        self.outputs[output_idx].active_tag = tag;
        if old != tag {
            self.clear_fullscreen_on_tag(old);
        }
        self.refocus_output(output_idx);
    }

    /// Un-fullscreen every window on `tag` (they are no longer visible).
    pub fn clear_fullscreen_on_tag(&mut self, tag: usize) {
        for w in self.windows.iter_mut().filter(|w| w.tag == tag) {
            w.fullscreen = false;
        }
    }

    /// Drop render nodes of windows that are not on an active tag (and so are
    /// not on any output). Nodes are only created for visible windows, so
    /// keeping them alive leaks one `river_node_v1` per window hidden by a tag
    /// switch. Must run inside a render sequence: `river_window_v1.get_node`
    /// is an error while a stale node exists.
    pub fn release_hidden_nodes(&mut self) {
        let active: Vec<usize> = self.outputs.iter().map(|o| o.active_tag).collect();
        for window in self.windows.iter_mut() {
            if window.node.is_some() && !active.contains(&window.tag) {
                window.node = None;
            }
        }
    }

    /// Move the focused window of an output to a global tag. Unassigned tags
    /// are created (assigned) on the output first. Moving to the tag the window
    /// is already on is a no-op (it would otherwise unassign the active tag).
    pub fn send_focused_to_tag(&mut self, output_idx: usize, tag: usize) {
        if tag >= self.tags.len() || output_idx >= self.outputs.len() {
            return;
        }
        let focused = self.outputs[output_idx].focused_window;
        let Some(fid) = focused.filter(|fid| {
            self.find_window(*fid)
                .is_some_and(|w| w.tag != tag)
        }) else {
            return;
        };
        if self.tags[tag].output.is_none() {
            self.tags[tag].output = Some(output_idx);
        }
        if let Some(w) = self.find_window_mut(fid) {
            w.tag = tag;
        }
        self.refocus_output(output_idx);
        self.mark_dirty();
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

pub(crate) fn clamp_rect_to_area(
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    area_x: i32,
    area_y: i32,
    area_width: u32,
    area_height: u32,
) -> (i32, i32) {
    if area_width == 0 || area_height == 0 {
        return (area_x, area_y);
    }

    let max_x = area_x + area_width.saturating_sub(width.min(area_width)) as i32;
    let max_y = area_y + area_height.saturating_sub(height.min(area_height)) as i32;
    (x.clamp(area_x, max_x), y.clamp(area_y, max_y))
}

/// Size a child window that overlays its parent: at most 3/5 of the parent's
/// width and height (so the parent stays clickable), clamped to the child's
/// min/max hints.
///
/// A child that asks for the whole output (xdg-dialog, portal pickers, some
/// GTK apps) cannot sit beside its parent at its requested size; shrinking it
/// keeps both windows readable. The result never exceeds the child's max hint
/// but may go below its min hint, because overlapping the parent is worse.
/// `parent` is the rect the child overlays; `req`/`hints` are the child's
/// requested size and `(min_w, min_h, max_w, max_h)`.
pub fn fit_beside_parent(
    parent: (u32, u32),
    req: (u32, u32),
    hints: (u32, u32, u32, u32),
) -> (u32, u32) {
    let cap = |requested: u32, area: u32, min: u32, max: u32| -> u32 {
        let max_side = if area > 0 { area * 3 / 5 } else { requested };
        let mut out = requested.min(max_side).max(min);
        if max > 0 {
            out = out.min(max);
        }
        out
    };
    (
        cap(req.0, parent.0, hints.0, hints.2),
        cap(req.1, parent.1, hints.1, hints.3),
    )
}

/// Centre `w`x`h` over `(x, y, width, height)`, keeping the result inside
/// `(area_x, area_y, area_width, area_height)` via [`clamp_rect_to_area`].
pub fn place_over_parent(
    child_w: u32,
    child_h: u32,
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    area: (i32, i32, u32, u32),
) -> (i32, i32) {
    let cx = x + width as i32 / 2 - child_w as i32 / 2;
    let cy = y + height as i32 / 2 - child_h as i32 / 2;
    clamp_rect_to_area(cx, cy, child_w, child_h, area.0, area.1, area.2, area.3)
}

/// The area floating windows are laid out in: the usable (panel-free) area,
/// or the whole output before river reports one.
pub fn output_area(output: &Output) -> (i32, i32, u32, u32) {
    if output.usable_width > 0 || output.usable_height > 0 {
        (output.usable_x, output.usable_y, output.usable_width, output.usable_height)
    } else {
        (output.x, output.y, output.width, output.height)
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

    #[test]
    fn master_fraction_is_per_tag_and_clamped() {
        let mut state = State::new(0.55);
        assert_eq!(state.master_fraction(0), 0.55);
        assert_eq!(state.master_fraction(8), 0.55);

        // Changing one tag does not affect another.
        state.set_master_fraction(0, 0.8);
        assert_eq!(state.master_fraction(0), 0.8);
        assert_eq!(state.master_fraction(1), 0.55);

        // Out-of-range fractions are clamped.
        state.set_master_fraction(2, 5.0);
        assert_eq!(state.master_fraction(2), 0.9);
        state.set_master_fraction(2, -1.0);
        assert_eq!(state.master_fraction(2), 0.1);
    }

    #[test]
    fn resize_mode_toggles_both_ways() {
        let mut state = State::new(0.55);
        assert!(!state.resize_mode);
        assert!(state.toggle_resize_mode());
        assert!(state.resize_mode);
        assert!(!state.toggle_resize_mode());
        assert!(!state.resize_mode);
    }

    #[test]
    fn clamp_rect_keeps_floating_window_inside_output_area() {
        assert_eq!(
            clamp_rect_to_area(2500, -200, 400, 300, 100, 50, 900, 700),
            (600, 50)
        );
        assert_eq!(
            clamp_rect_to_area(-20, 900, 1200, 800, 100, 50, 900, 700),
            (100, 50)
        );
    }

    #[test]
    fn clamp_floating_windows_zero_allocation() {
        let mut state = State::new(0.55);
        state.clamp_floating_windows();
        assert!(state.windows.is_empty());
    }

    #[test]
    fn best_tag_for_output_uses_bitmask() {
        let mut state = State::new(0.55);
        state.tags[0].output = Some(0);
        state.tags[1].output = Some(0);
        assert_eq!(state.best_tag_for_output(0), None);
    }

    /// Tag/window bookkeeping that does not need Wayland objects.
    fn tags_only(outputs: usize) -> State {
        let mut state = State::new(0.55);
        for i in 0..outputs {
            state.outputs.push(output_slot());
            state.assign_initial_tag(i);
        }
        state.focused_output = Some(0);
        state
    }

    /// An `Output` whose proxy is never sent on: tag bookkeeping only.
    fn output_slot() -> Output {
        Output::new(null_proxy())
    }

    /// A proxy on a client backend with no server on the other end. Safe to
    /// hold; tests never flush to it. All proxies share one connection so
    /// dropping one does not take the others with it.
    fn null_proxy<P: wayland_client::Proxy>() -> P {
        thread_local! {
            static CONN: wayland_client::Connection = wayland_client::Connection::from_backend(
                wayland_backend::rs::client::Backend::connect(
                    std::os::unix::net::UnixStream::pair().unwrap().0,
                )
                .unwrap(),
            );
        }
        P::from_id(&CONN.with(|c| c.clone()), wayland_client::backend::ObjectId::null()).unwrap()
    }

    #[test]
    fn focus_tag_ignores_out_of_range_tag() {
        // The status socket can send a tag past the tag table; storing it
        // would make `1u32 << tag` in the snapshot builder panic.
        let mut state = tags_only(1);
        state.focus_tag(0, 9);
        state.focus_tag(0, 1 << 20);
        assert_eq!(state.outputs[0].active_tag, 0);
        assert_eq!(state.tag_owner(0), Some(0));
    }

    #[test]
    fn focus_tag_assigns_unassigned_tag_to_the_focused_output() {
        let mut state = tags_only(2);
        state.focus_tag(1, 5);
        assert_eq!(state.tag_owner(5), Some(1));
        assert_eq!(state.outputs[1].active_tag, 5);
        assert_eq!(state.focused_output, Some(0), "own tag stays on its output");

        // A tag owned by another output moves focus there instead.
        state.focus_tag(0, 5);
        assert_eq!(state.focused_output, Some(1));
    }

    #[test]
    fn send_to_the_current_tag_is_a_no_op() {
        // Moving a window to the tag it is already on used to unassign that
        // tag as "empty" and leave every window on it invisible.
        let mut state = tags_only(1);
        let mut w = null_window(0);
        w.tag = 1;
        state.tags[1].output = Some(0);
        let wid = w.id;
        state.windows.push(w);
        state.outputs[0].active_tag = 1;
        state.outputs[0].focused_window = Some(wid);

        state.send_focused_to_tag(0, 1);
        assert_eq!(state.tag_owner(1), Some(0));
        assert!(state.window_is_visible(state.find_window(wid).unwrap()));

        // Untagged windows are not drawn, so the tag left behind is gone.
        state.send_focused_to_tag(0, 2);
        assert_eq!(state.tag_owner(1), Some(0), "ownership is kept for the window");
        assert!(!state.window_is_visible(state.find_window(wid).unwrap()));
        assert_eq!(state.find_window(wid).unwrap().tag, 2);
        assert_eq!(state.tag_owner(2), Some(0));
    }

    #[test]
    fn an_emptied_tag_keeps_its_output() {
        // d640386 deleted a tag emptied by send_to_tag. The output was then
        // tagless, and normalize_outputs gives a tagless output the lowest
        // free tag index, so windows went to a tag the user never opened.
        let mut state = tags_only(1);
        let mut w = null_window(0);
        w.tag = 0;
        let wid = w.id;
        state.windows.push(w);
        state.outputs[0].focused_window = Some(wid);
        state.send_focused_to_tag(0, 1);
        assert_eq!(state.tag_owner(0), Some(0));
        assert_eq!(state.outputs[0].active_tag, 0);
    }

    #[test]
    fn empty_tag_loses_its_output_but_comes_back_there() {
        // The output must return to the tag it was on, not to a free index.
        let mut state = tags_only(1);
        state.focus_tag(0, 1);
        assert_eq!(state.focus_tag(0, 0), ());
        assert_eq!(state.tag_owner(1), Some(0), "1 still has the output");
        state.focus_tag(0, 1);
        state.focus_tag(0, 0);
        assert_eq!(state.outputs[0].active_tag, 0, "back on 0, not on a free tag");
        assert_eq!(state.tag_owner(1), Some(0), "the tag left is kept");
    }

    fn null_window(tag: usize) -> Window {
        Window::new(
            null_proxy::<crate::protocols::wm::river_window_v1::RiverWindowV1>(),
            tag,
        )
    }

    #[test]
    fn release_hidden_nodes_drops_nodes_of_inactive_tags_only() {
        let mut state = tags_only(1);
        let mut a = null_window(0);
        let mut b = null_window(1);
        state.tags[0].output = Some(0);
        state.tags[1].output = Some(0);
        state.outputs[0].active_tag = 0;
        a.node = Some(null_proxy::<crate::protocols::wm::river_node_v1::RiverNodeV1>());
        b.node = Some(null_proxy::<crate::protocols::wm::river_node_v1::RiverNodeV1>());
        let (aid, bid) = (a.id, b.id);
        state.windows.push(a);
        state.windows.push(b);

        state.release_hidden_nodes();
        assert!(state.find_window(aid).unwrap().node.is_some());
        assert!(state.find_window(bid).unwrap().node.is_none());
    }

    #[test]
    fn floating_window_is_clamped_to_the_output_it_is_shown_on() {
        // Window on a tag with no owner: it is drawn on the focused output,
        // so it must be clamped there and not to output 0.
        let mut state = tags_only(2);
        state.outputs[1].x = 1920;
        state.outputs[1].width = 2560;
        state.outputs[1].height = 1440;
        let mut w = null_window(8);
        w.floating = true;
        w.float_x = 100; // inside output 0, outside output 1
        w.float_w = 200;
        w.float_h = 100;
        state.windows.push(w);
        state.focused_output = Some(1);

        state.clamp_floating_windows();
        assert_eq!(state.windows[0].float_x, 1920);
    }

    #[test]
    fn output_area_falls_back_until_usable_area_is_known() {
        let mut out = output_slot();
        out.x = 1920;
        out.y = 0;
        out.width = 2560;
        out.height = 1440;
        assert_eq!(output_area(&out), (1920, 0, 2560, 1440));

        out.usable_x = 1920;
        out.usable_y = 40;
        out.usable_width = 2560;
        out.usable_height = 1400;
        assert_eq!(output_area(&out), (1920, 40, 2560, 1400));
    }

    #[test]
    fn dialog_is_sized_beside_its_parent_and_clamped_to_min() {
        // A GPS-style dialog asking for the whole output above a 1000x600
        // parent: capped at 3/5 of the parent, not at the parent's size.
        assert_eq!(fit_beside_parent((1000, 600), (1000, 600), (0, 0, 0, 0)), (600, 360));
        // The min hint wins over the cap.
        assert_eq!(fit_beside_parent((1000, 600), (1000, 600), (700, 0, 0, 0)), (700, 360));
        // A max hint below the cap wins.
        assert_eq!(fit_beside_parent((1000, 600), (1000, 600), (0, 0, 300, 0)), (300, 360));
        // Small dialogs are left alone.
        assert_eq!(fit_beside_parent((1000, 600), (300, 200), (0, 0, 0, 0)), (300, 200));
    }

    #[test]
    fn dialog_placed_over_parent_stays_inside_the_output() {
        let (x, y) = place_over_parent(200, 100, 0, 0, 200, 100, (1920, 0, 1000, 800));
        assert!(x >= 1920 && x + 200 <= 2920 && y + 100 <= 800);
    }
}
