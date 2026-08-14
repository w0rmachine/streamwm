//! Tiling layout computation and window positioning.

use crate::config::Config;
use crate::connection::AppData;
use crate::state::State;

/// A window geometry in logical pixels (global coordinates).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Geometry {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

/// Fraction of the width given to the master window (main+stack layout).
const MASTER_FRACTION: f64 = 0.55;

/// Compute geometry for every visible window across all outputs.
/// Returns (window_id, geometry) — floating windows are excluded (they are
/// positioned independently).
pub fn compute_all(state: &State, config: &Config) -> Vec<(u32, Geometry)> {
    let mut out = Vec::new();
    for (idx, _output) in state.outputs.iter().enumerate() {
        out.extend(compute_output(state, idx, config));
    }
    out
}

/// Visible (non-floating) windows for an output, in master+stack order.
fn visible_windows(state: &State, output_idx: usize) -> Vec<u32> {
    let active = state.outputs[output_idx].active_mask;
    let mut ids: Vec<u32> = state
        .windows
        .iter()
        .filter(|w| w.output == output_idx && (active >> w.tag) & 1 == 1 && !w.floating)
        .map(|w| w.id)
        .collect();
    // Focused window first (becomes the master).
    if let Some(fid) = state.outputs[output_idx].focused_window {
        if let Some(pos) = ids.iter().position(|id| *id == fid) {
            let w = ids.remove(pos);
            ids.insert(0, w);
        }
    }
    ids
}

fn compute_output(state: &State, output_idx: usize, config: &Config) -> Vec<(u32, Geometry)> {
    let output = &state.outputs[output_idx];
    let ids = visible_windows(state, output_idx);
    if ids.is_empty() {
        return Vec::new();
    }

    let gap = config.gap as i32;

    let base_x = if output.usable_width > 0 || output.usable_height > 0 {
        output.usable_x
    } else {
        output.x
    };
    let base_y = if output.usable_width > 0 || output.usable_height > 0 {
        output.usable_y
    } else {
        output.y
    };
    let base_w = if output.usable_width > 0 || output.usable_height > 0 {
        output.usable_width
    } else {
        output.width
    };
    let base_h = if output.usable_width > 0 || output.usable_height > 0 {
        output.usable_height
    } else {
        output.height
    };

    let area_x = base_x + gap;
    let area_y = base_y + gap;
    let area_w = (base_w as i32 - gap * 2).max(0) as u32;
    let area_h = (base_h as i32 - gap * 2).max(0) as u32;

    compute_tiling(
        &ids,
        Geometry {
            x: area_x,
            y: area_y,
            width: area_w,
            height: area_h,
        },
        base_y + base_h as i32,
        gap,
    )
}

fn compute_tiling(
    ids: &[u32],
    area: Geometry,
    output_bottom: i32,
    gap: i32,
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

    let master_w = ((area.width as f64 * MASTER_FRACTION) as i32).max(0) as u32;
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
        let h = if i + 1 == n - 1 {
            (output_bottom - gap - y).max(0) as u32
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

/// Position windows and set borders during a render sequence.
pub fn render_all_run(data: &mut AppData) {
    let config = data.config.clone();
    let geometries = {
        let state = data.state.borrow();
        compute_all(&state, &config)
    };

    let focused = {
        let state = data.state.borrow();
        state
            .active_output()
            .and_then(|o| state.outputs[o].focused_window)
    };

    let (border_r, border_g, border_b) = config.color(&config.border_color);
    let (f_r, f_g, f_b) = config.color(&config.focused_border_color);

    // Decide visibility and geometry for each window, then apply.
    let decisions: Vec<(u32, bool, Option<Geometry>)> = {
        let state = data.state.borrow();
        state
            .windows
            .iter()
            .map(|w| {
                let geom = geometries
                    .iter()
                    .find(|(wid, _)| *wid == w.id)
                    .map(|(_, g)| *g);
                let floating_visible = w.floating
                    && state
                        .outputs
                        .get(w.output)
                        .map(|o| (o.active_mask >> w.tag) & 1 == 1)
                        .unwrap_or(false);
                (w.id, geom.is_some() || floating_visible, geom)
            })
            .collect()
    };

    let mut state = data.state.borrow_mut();
    for (wid, visible, geom) in decisions {
        let Some(window) = state.find_window_mut(wid) else {
            continue;
        };
        let is_focused = focused == Some(wid);

        if !visible {
            window.proxy.hide();
            continue;
        }

        // Position via the render node.
        if let Some(node) = &window.node {
            if let Some(g) = geom {
                node.set_position(g.x, g.y);
            } else if window.floating {
                node.set_position(window.float_x, window.float_y);
            }
            node.place_top();
        }

        // Borders.
        let (r, g, b) = if is_focused {
            (f_r, f_g, f_b)
        } else {
            (border_r, border_g, border_b)
        };
        let edges = crate::protocols::wm::river_window_v1::Edges::Top
            | crate::protocols::wm::river_window_v1::Edges::Bottom
            | crate::protocols::wm::river_window_v1::Edges::Left
            | crate::protocols::wm::river_window_v1::Edges::Right;
        let (a, r32, g32, b32) = to_32bit(r, g, b);
        window
            .proxy
            .set_borders(edges, config.border_width as i32, r32, g32, b32, a);
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

    #[test]
    fn tiling_empty_has_no_geometry() {
        assert!(compute_tiling(
            &[],
            Geometry {
                x: 0,
                y: 0,
                width: 100,
                height: 100
            },
            100,
            4
        )
        .is_empty());
    }

    #[test]
    fn tiling_single_window_fills_area() {
        let area = Geometry {
            x: 10,
            y: 20,
            width: 800,
            height: 600,
        };

        assert_eq!(compute_tiling(&[7], area, 620, 8), vec![(7, area)]);
    }

    #[test]
    fn tiling_three_windows_uses_master_and_even_stack() {
        let area = Geometry {
            x: 10,
            y: 10,
            width: 980,
            height: 580,
        };

        assert_eq!(
            compute_tiling(&[1, 2, 3], area, 600, 10),
            vec![
                (
                    1,
                    Geometry {
                        x: 10,
                        y: 10,
                        width: 539,
                        height: 580,
                    },
                ),
                (
                    2,
                    Geometry {
                        x: 559,
                        y: 10,
                        width: 431,
                        height: 285,
                    },
                ),
                (
                    3,
                    Geometry {
                        x: 559,
                        y: 305,
                        width: 431,
                        height: 285,
                    },
                ),
            ]
        );
    }

    #[test]
    fn color_conversion_maps_8_bit_to_protocol_range() {
        assert_eq!(
            to_32bit(0, 127, 255),
            (0xFFFF_FFFF, 0, 0x7F7F_7F7F, 0xFFFF_FFFF)
        );
    }
}
