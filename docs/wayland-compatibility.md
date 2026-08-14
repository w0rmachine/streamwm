# Wayland/River Compatibility Notes

streamwm is a Wayland client that owns river window-management policy through
`river-window-management-v1` version 4. River remains the compositor. streamwm
must obey river's manage/render state split or river may raise protocol errors.

## Protocol Objects

- `river_window_manager_v1`: one global, bound at startup. Drives manage/render
  sequences.
- `river_window_v1`: one object per managed toplevel/Xwayland window.
- `river_output_v1`: one logical output. Each output is matched to a `wl_output`
  through the `river_output_v1.wl_output` global id.
- `river_seat_v1`: one input seat. Used for keyboard focus and pointer events.
- `river_xkb_bindings_v1`: optional. Used for compositor-owned keybindings.
- `river_layer_shell_v1`: optional. Binding it tells river this WM supports
  layer shell clients such as bars/backgrounds.

## Sequence Rules

River splits state into two groups:

- Window-management state: `propose_dimensions`, `focus_window`,
  `clear_focus`, `close`, `fullscreen`, `exit_fullscreen`, `use_ssd`,
  `use_csd`, keybinding enable/disable.
- Render state: `hide`, `show`, `set_borders`, node position/order.

Window-management requests are legal only between `manage_start` and
`manage_finish`.

Render requests are legal between `manage_start` and `manage_finish`, or between
`render_start` and `render_finish`.

Current streamwm flow:

1. `manage_start`
2. Recompute tag occupancy/focus.
3. Hide all windows to avoid river's default "new windows are shown" behavior.
4. Apply deferred close/fullscreen/decoration requests.
5. Set layer-shell default output once.
6. Focus the selected window for every seat.
7. Propose dimensions for visible tiled windows.
8. Enable keybindings.
9. `manage_finish`
10. `render_start`
11. Create render nodes once.
12. Position visible windows, set borders, show visible windows, hide the rest.
13. `render_finish`

## Tag And Output Model

Tags are per output. A window stores:

- `output`: index into `State.outputs`
- `tag`: tag id `0..=9`

Layout, focus cycling, occupied masks, status snapshots, and floating visibility
must filter on both fields. Tag `1` on output A is not tag `1` on output B.

New windows are assigned to the focused output and that output's first active
tag before the first manage pass. This prevents a new window from appearing on
the wrong tag/output.

## Output Names

river sends `river_output_v1.wl_output { name }`, where `name` is the Wayland
registry global id. streamwm binds that exact `wl_output`, listens for
`wl_output.name`, then stores names such as `eDP-1` or `DP-1`.

The status/control command `focus_output` depends on these names. Without this
binding, only fallback names like `output-0` exist.

## Decoration Policy

`Config.use_ssd` maps to river `use_ssd`/`use_csd` during manage sequences.
river ignores `use_ssd` for clients that only support CSD, which is compliant
with the protocol.

## Known Gaps

- `dimensions_hint`, pointer move/resize requests, maximize/minimize requests,
  and presentation hints are not yet acted on.
- Floating windows use a fixed default rectangle until move/resize support is
  implemented.
- Status socket is private only by filesystem path under `XDG_RUNTIME_DIR`; keep
  `allow_spawn = false` if untrusted local processes can reach it.
