//! Master-Stack tiling layout calculation and window rendering operations.
//!
//! Calculates precise window geometry boundaries for tiled surfaces based on:
//! - Active tag master width fraction (`0.1..=0.9`).
//! - Configured window gap spacing (`gap`).
//! - Screen usable area (accounting for layer-shell panels).
//! - Floating window overlay z-ordering (tiled windows rendered first, floating windows rendered on top).

use crate::config::Config;
use crate::state::{output_area, State};

/// Window rectangular geometry represented in global screen coordinates (logical pixels).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Geometry {
    /// X coordinate origin.
    pub x: i32,
    /// Y coordinate origin.
    pub y: i32,
    /// Logical pixel width.
    pub width: u32,
    /// Logical pixel height.
    pub height: u32,
}

/// Computes tiling layout geometry for every visible non-floating window across all active outputs.
/// Returns a vector of `(streamwm_window_id, Geometry)` tuples.
pub fn compute_all(state: &State, config: &Config) -> Vec<(u32, Geometry)> {
    let mut out = Vec::new();
    for (idx, _output) in state.outputs.iter().enumerate() {
        out.extend(compute_output(state, idx, config));
    }
    out
}

/// Geometry of one window in the current layout, if it is tiled and visible.
pub fn geometry_of(state: &State, config: &Config, id: u32) -> Option<Geometry> {
    compute_all(state, config)
        .into_iter()
        .find(|(wid, _)| *wid == id)
        .map(|(_, g)| g)
}

/// Visible (non-floating) windows for an output, in master+stack order.
fn visible_windows(state: &State, output_idx: usize) -> Vec<u32> {
    let active = state.outputs[output_idx].active_tag;
    state
        .windows
        .iter()
        .filter(|w| state.tag_owner(w.tag) == Some(output_idx) && w.tag == active && !w.floating)
        .map(|w| w.id)
        .collect()
}

fn compute_output(state: &State, output_idx: usize, config: &Config) -> Vec<(u32, Geometry)> {
    let output = &state.outputs[output_idx];
    let ids = visible_windows(state, output_idx);
    if ids.is_empty() {
        return Vec::new();
    }

    let gap = config.gap as i32;
    let (base_x, base_y, base_w, base_h) = output_area(output);
    let area_x = base_x + gap;
    let area_y = base_y + gap;
    let area_w = (base_w as i32 - gap * 2).max(0) as u32;
    let area_h = (base_h as i32 - gap * 2).max(0) as u32;

    let master_fraction = state.active_master_fraction(output_idx);

    // The last stack tile reaches the bottom of the usable area (not the
    // output), so a bottom panel does not push windows off screen.
    let bottom = area_y + area_h as i32;

    compute_tiling(
        &ids,
        Geometry {
            x: area_x,
            y: area_y,
            width: area_w,
            height: area_h,
        },
        bottom,
        gap,
        master_fraction,
    )
}

fn compute_tiling(
    ids: &[u32],
    area: Geometry,
    area_bottom: i32,
    gap: i32,
    master_fraction: f64,
) -> Vec<(u32, Geometry)> {
    let n = ids.len();
    let mut result = Vec::with_capacity(n);

    if ids.is_empty() {
        return result;
    }

    if n == 1 {
        result.push((ids[0], area));
        return result;
    }

    let master_w = ((area.width as f64 * master_fraction) as i32).max(0) as u32;
    result.push((
        ids[0],
        Geometry {
            x: area.x,
            y: area.y,
            width: master_w,
            height: area.height,
        },
    ));

    let stack_x = area.x + master_w as i32 + gap;
    let stack_w = (area.width as i32 - master_w as i32 - gap).max(0) as u32;
    let stack_n = (n - 1) as u32;
    let stack_h = (area
        .height
        .saturating_sub((stack_n.saturating_sub(1)) * gap as u32))
        / stack_n.max(1);

    for (i, id) in ids.iter().skip(1).enumerate() {
        let y = area.y + i as i32 * (stack_h as i32 + gap);
        // The last tile absorbs the rounding remainder so the stack ends
        // exactly on the area's bottom edge.
        let h = if i + 1 == n - 1 {
            (area_bottom - gap - y).max(0) as u32
        } else {
            stack_h
        };
        result.push((
            *id,
            Geometry {
                x: stack_x,
                y,
                width: stack_w,
                height: h,
            },
        ));
    }

    result
}

/// Which render request a window belongs in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    /// Not shown this frame (minimised, or on an inactive tag).
    Hidden,
    /// Fullscreen: river positions it; only z-order and borders apply.
    Fullscreen,
    /// Positioned at the given geometry (tiled cell or float box).
    At(Geometry),
}

/// Decide placement for every window, in the order they must be placed:
/// hidden windows first, then tiled, then floating windows last so floats end
/// up on top.
///
/// Order is computed here rather than from the protocol's per-window
/// dimensions events (whose order depends on client round-trips).
pub fn placements(state: &State, config: &Config) -> Vec<(u32, Placement)> {
    let tiled: Vec<(u32, Geometry)> = compute_all(state, config);

    let (mut hidden, mut tiled_out, mut floats) = (Vec::new(), Vec::new(), Vec::new());
    for w in &state.windows {
        let tiled_geom = tiled.iter().find(|(wid, _)| *wid == w.id).map(|(_, g)| *g);
        let placement = match tiled_geom {
            _ if w.minimized || !state.window_is_visible(w) => Placement::Hidden,
            _ if w.fullscreen => Placement::Fullscreen,
            Some(g) => Placement::At(g),
            None => Placement::At(Geometry {
                x: w.float_x,
                y: w.float_y,
                width: w.float_w,
                height: w.float_h,
            }),
        };
        if placement == Placement::Hidden {
            hidden.push((w.id, placement));
        } else if w.floating {
            floats.push((w.id, placement));
        } else {
            tiled_out.push((w.id, placement));
        }
    }
    hidden.extend(tiled_out);
    hidden.extend(floats);
    hidden
}

/// Apply placements during a render sequence.
pub fn render_all_run(state: &mut State, config: &Config, focused: Option<u32>) {
    state.release_hidden_nodes();
    let (border_r, border_g, border_b) = config.color(&config.border_color);
    let (f_r, f_g, f_b) = config.color(&config.focused_border_color);
    let edges = crate::protocols::wm::river_window_v1::Edges::Top
        | crate::protocols::wm::river_window_v1::Edges::Bottom
        | crate::protocols::wm::river_window_v1::Edges::Left
        | crate::protocols::wm::river_window_v1::Edges::Right;

    for (wid, placement) in placements(state, config) {
        let Some(window) = state.find_window_mut(wid) else {
            continue;
        };
        if placement == Placement::Hidden {
            window.proxy.hide();
            continue;
        }

        if let Some(node) = &window.node {
            match placement {
                Placement::Fullscreen => node.set_position(0, 0),
                Placement::At(g) => node.set_position(g.x, g.y),
                Placement::Hidden => unreachable!(),
            }
            node.place_top();
        }

        let is_focused = focused == Some(wid);
        let (r, g, b) = if is_focused {
            (f_r, f_g, f_b)
        } else {
            (border_r, border_g, border_b)
        };
        let border_state = (is_focused, config.border_width, r, g, b);
        if window.border_applied != Some(border_state) {
            let (a, r32, g32, b32) = to_32bit(r, g, b);
            window
                .proxy
                .set_borders(edges, config.border_width as i32, r32, g32, b32, a);
            window.border_applied = Some(border_state);
        }
        window.proxy.show();
    }
}

/// Convert 8-bit color components to river's 32-bit RGBA (pre-multiplied).
fn to_32bit(r: u8, g: u8, b: u8) -> (u32, u32, u32, u32) {
    let f = |v: u8| -> u32 { ((v as u64 * 0xFFFF_FFFFu64) / 255) as u32 };
    (f(0xff), f(r), f(g), f(b))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area(x: i32, y: i32, width: u32, height: u32) -> Geometry {
        Geometry {
            x,
            y,
            width,
            height,
        }
    }

    #[test]
    fn tiling_empty_has_no_geometry() {
        assert!(compute_tiling(&[], area(0, 0, 100, 100), 100, 4, 0.55).is_empty());
    }

    #[test]
    fn tiling_single_window_fills_area() {
        let a = area(10, 20, 800, 600);
        assert_eq!(compute_tiling(&[7], a, 620, 8, 0.55), vec![(7, a)]);
    }

    #[test]
    fn tiling_three_windows_uses_master_and_even_stack() {
        assert_eq!(
            compute_tiling(&[1, 2, 3], area(10, 10, 980, 580), 590, 10, 0.55),
            vec![
                (1, area(10, 10, 539, 580)),
                (2, area(559, 10, 431, 285)),
                // Ends exactly at the usable bottom (590) minus the gap.
                (3, area(559, 305, 431, 275)),
            ]
        );
    }

    #[test]
    fn last_stack_tile_stops_at_usable_bottom_not_output_bottom() {
        // 10px panel below the usable area: the stack must end above it.
        let tiles = compute_tiling(&[1, 2, 3], area(10, 10, 980, 580), 590, 10, 0.55);
        let last = tiles.last().unwrap().1;
        assert_eq!(last.y + last.height as i32, 590 - 10);
    }

    mod placement {
        use super::super::*;
        use crate::protocols::wm::river_node_v1::RiverNodeV1;
        use crate::protocols::wm::river_output_v1::RiverOutputV1;
        use crate::protocols::wm::river_window_v1::RiverWindowV1;
        use crate::state::Window;

        fn conn() -> wayland_client::Connection {
            thread_local! {
                static CONN: wayland_client::Connection = wayland_client::Connection::from_backend(
                    wayland_backend::rs::client::Backend::connect(
                        std::os::unix::net::UnixStream::pair().unwrap().0,
                    )
                    .unwrap(),
                );
            }
            CONN.with(|c| c.clone())
        }

        fn proxy<P: wayland_client::Proxy>() -> P {
            P::from_id(&conn(), wayland_client::backend::ObjectId::null()).unwrap()
        }

        /// One 1000x600 output with tag 0 active and no windows.
        fn state() -> State {
            let mut state = State::new(0.55);
            let mut out = crate::state::Output::new(proxy::<RiverOutputV1>());
            out.name = Some("DP-1".into());
            out.width = 1000;
            out.height = 600;
            state.outputs.push(out);
            state.assign_initial_tag(0);
            state
        }

        fn window(state: &mut State, tag: usize) -> u32 {
            let w = Window::new(proxy::<RiverWindowV1>(), tag);
            let id = w.id;
            state.windows.push(w);
            id
        }

        #[test]
        fn layout_uses_the_full_output_until_the_usable_area_is_reported() {
            // river sends `non_exclusive_area` late; before that, tiling into
            // a 0x0 area would put every window at zero size on that output.
            let mut state = state();
            let config = Config::default();
            let id = window(&mut state, 0);
            let g = geometry_of(&state, &config, id).expect("window is tiled");
            assert!(g.width > 0 && g.height > 0);
        }

        #[test]
        fn floats_are_placed_last_and_hidden_windows_are_not_placed() {
            let mut state = state();
            let config = Config::default();
            let tiled = window(&mut state, 0);
            let float = window(&mut state, 0);
            state.find_window_mut(float).unwrap().floating = true;
            state.find_window_mut(float).unwrap().float_x = 10;
            state.find_window_mut(float).unwrap().float_y = 20;
            let hidden = window(&mut state, 0);
            state.find_window_mut(hidden).unwrap().minimized = true;
            // Off-tag window: not on the active tag, so not rendered.
            let other = window(&mut state, 1);

            let p = placements(&state, &config);
            // Hidden windows first (an explicit hide each frame), then tiled,
            // then floats last so they end up on top.
            assert_eq!(
                p,
                vec![
                    (hidden, Placement::Hidden),
                    (other, Placement::Hidden),
                    (
                        tiled,
                        Placement::At(geometry_of(&state, &config, tiled).unwrap())
                    ),
                    (
                        float,
                        Placement::At(Geometry {
                            x: 10,
                            y: 20,
                            width: 400,
                            height: 300,
                        }),
                    ),
                ]
            );
        }

        #[test]
        fn fullscreen_window_is_left_to_the_compositor() {
            let mut state = state();
            let config = Config::default();
            let id = window(&mut state, 0);
            state.find_window_mut(id).unwrap().fullscreen = true;
            let p = placements(&state, &config);
            assert_eq!(p, vec![(id, Placement::Fullscreen)]);
        }

        #[test]
        fn releasing_nodes_happens_before_get_node_in_render() {
            // `get_node` twice is a protocol error; the render pass must drop
            // nodes of windows that left the active tag.
            let mut state = state();
            let id = window(&mut state, 0);
            state.tags[1].output = Some(0);
            state.find_window_mut(id).unwrap().node = Some(proxy::<RiverNodeV1>());
            state.windows[0].tag = 1;
            state.release_hidden_nodes();
            assert!(state.find_window(id).unwrap().node.is_none());
        }
    }

    #[test]
    fn color_conversion_maps_8_bit_to_protocol_range() {
        assert_eq!(
            to_32bit(0, 127, 255),
            (0xFFFF_FFFF, 0, 0x7F7F_7F7F, 0xFFFF_FFFF)
        );
    }
}
