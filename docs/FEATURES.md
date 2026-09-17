# streamwm — Technical Features & Architecture Specification

`streamwm` is a dynamic tiling window manager client written in Rust for the [`river`](https://isaacfreund.com/software/river) Wayland compositor.

---

## 1. Architectural Model & Protocol Split

`river` is a non-monolithic compositor: rather than bundling window management policy into the display server executable, it exposes the `river-window-management-v1` protocol to delegate window management rules to an external client (`streamwm`).

```
 +-----------------------------------------------------------------------+
 |                         river Wayland Compositor                     |
 |  (DRM Output, Input Drivers, wlr-layer-shell, Wayland Server Loop)  |
 +-----------------------------------+-----------------------------------+
                                     |
             Wayland IPC Protocol    | `river-window-management-v1`
             Unix Domain Socket      | `river-xkb-bindings-v1`
                                     | `river-layer-shell-v1`
 +-----------------------------------+-----------------------------------+
 |                             streamwm                                  |
 |  (Window Manager Policy, Tiling Engine, Tag Model, Hotkeys, IPC)   |
 +-----------------------------------------------------------------------+
```

### Double-Buffered Manage / Render Protocol Sequences
River enforces a strict protocol state split:
1. **Manage Sequence (`manage_start` -> `manage_finish`)**:
   - `propose_dimensions(w, h)`: Suggest content dimensions for visible tiled and floating windows.
   - `focus_window(proxy)` / `clear_focus()`: Route seat keyboard focus.
   - `close()`: Send client close requests.
   - `fullscreen(output)` / `exit_fullscreen()`: Toggle fullscreen window state.
   - `use_ssd()` / `use_csd()`: Set server-side vs client-side window titlebar decoration policy.
   - Hotkey enable (`binding.enable()`) / disable (`binding.disable()`).
2. **Render Sequence (`render_start` -> `render_finish`)**:
   - `river_node_v1.set_position(x, y)`: Position windows in screen space.
   - `river_node_v1.place_top()`: Adjust surface z-index hierarchy.
   - `set_borders(edges, width, r, g, b, a)`: Paint 32-bit premultiplied RGBA window borders.
   - `hide()` / `show()`: Control surface visibility.

---

## 2. Feature Matrix

### Unified Global Tag Engine (Tags 1–9)
- **Global Scope**: Tags 1 through 9 (`0..=8` internally) are shared across all connected monitors.
- **Dynamic Tag Ownership**: Each tag belongs to at most one output at any time. Focusing an unassigned tag creates (assigns) it on the current output.
- **Seamless Monitor Migration**: Unplugging or disabling a monitor automatically migrates all its owned tags to the first remaining output (`output 0`).
- **Automatic Empty Tag Garbage Collection**: Switching focus away from an empty tag (containing zero windows) unassigns it.
- **Cross-Output Navigation**: Focusing a tag owned by another monitor switches seat focus and warps the mouse cursor to that output automatically.

### Master-Stack Tiling Layout
- **Master Window**: Positioned on the left side, occupying a configurable width fraction (`0.1..=0.9`, default `0.55`).
- **Stack Windows**: Vertically stacked on the right side. Integer height division remainder is compensated on the bottom-most stack window to prevent sub-pixel layout gaps.
- **Per-Tag Layout Memory**: Each tag remembers its own master fraction split independently. Adjusting the split in resize mode on tag 1 does not alter tag 2.
- **Visual Spacing**: Configurable window gaps (`gap`), border width (`border_width`), and hex colors for focused vs unfocused borders.

### Interactive Floating Window Support
- **Rule-Based Auto-Floating**: Window `app_id` matches against `floating_app_ids` (e.g. `gnome-calculator`, `polkit-gnome`, Google Meet call window). Matching windows launch floating and centered.
- **Manual Toggle (`Mod+Shift+V`)**: Switches tiled windows to floating and back. Floating geometry is initialized from the window's current tiling cell.
- **Interactive Pointer Manipulation**:
  - `Mod + Left-Click-Drag`: Drag floating windows anywhere across monitors (`OpKind::Move`).
  - `Mod + Right-Click-Drag`: Interactive resizing of floating windows (`OpKind::Resize`).
- **Z-Index Layering**: Floating windows are rendered on top of tiled windows in the Wayland render node order.
- **Usable Area Clamping**: Floating window coordinates are clamped within monitor bounds, taking into account status bars and panels.

### Modal Resize State (`Mod+R`)
- Enters a dedicated modal resize state where `h`/`l` or `Left`/`Right` arrow keys adjust the master fraction split by `resize_step` (default `0.05`).
- **Keybinding Isolation**: Unmodified keysyms (`h`, `l`, `Left`, `Right`, `Escape`) are bound only while resize mode is active and disabled on `Escape`, preventing hotkey leaks into active terminal sessions.

### Layer-Shell Compatibility (`river-layer-shell-v1`)
- Signals support for `wlr-layer-shell` to River.
- Enables status bars (Waybar, Quickshell), launchers (Rofi, Fuzzel), and desktop wallpaper daemons (swaybg, hyprpaper).
- Dynamically respects `NonExclusiveArea` updates from status bars, recalculating usable screen area for window tiling.

### ACPI Lid-Switch & Display Topology Monitoring (`src/lid.rs`)
- Background polling thread watching `/proc/acpi/button/lid/LID0/state` and DRM connectors in `/sys/class/drm`.
- Independent of `logind` suspend policies.
- Automatically switches `kanshi` display output profiles on lid open/close, dock connection, or laptop undocking.
- Includes debounced DRM connector recovery (`wlr-randr --output <internal> --on --preferred`) to recover laptop screens after dock unplugging.

### JSON Domain Socket IPC (`src/status.rs`)
- Socket path: `$XDG_RUNTIME_DIR/streamwm-<display>.sock`.
- Line-delimited JSON protocol for external scripting and status bar integration.
- **Queries**: `get_status` returns JSON state snapshots (focused output, outputs, active/owned/occupied tag bitmasks, windows).
- **Commands**: `focus_tag`, `send_to_tag`, `focus_output`, `focus_window`, `spawn`, `quit`.
- **Security Guard**: `allow_spawn` config toggle protects socket execution of arbitrary binaries.

### Supervisor Auto-Restart Engine (`src/main.rs`)
- Parent supervisor process monitors the child worker process (`--streamwm-worker`).
- If the worker crashes due to a compositor disconnect or Wayland error, the supervisor automatically restarts it.
- Uses a rolling 60-second window tracking failures with bounded exponential backoff (250ms to 4s, capped at 5 restarts per minute) to prevent CPU thrashing.

### Low-Overhead Process Spawners (`src/wm/spawn.rs`)
- Command string analyzer detects special shell syntax characters (`|`, `&`, `$`, quotes, redirects).
- Simple executable commands launch directly via `execvp` without subshell overhead.
- Complex shell expressions transparently fall back to `$SHELL -c`.
- Background child process reaping threads prevent zombie process accumulation.

---

## 3. Configuration Reference (`config.toml`)

Default location: `/etc/streamwm/config.toml` or `~/.config/streamwm/config.toml`.

```toml
# Main modifier key: "super", "alt", or "ctrl"
modifier = "super"

# Terminal and launcher commands
terminal = "alacritty"
launcher = "fuzzel"

# Layout appearance
gap = 8
border_width = 2
border_color = "4c4c4c"
focused_border_color = "5294e2"

# Behaviors
focus_follows_mouse = true
use_ssd = true
allow_spawn = false

# Master split ratio and step size
master_fraction = 0.55
resize_step = 0.05

# Auto-floating application rules
floating_app_ids = [
  "polkit-gnome-authentication-agent-1",
  "org.gnome.Calculator",
  "google-meet"
]

# Custom keybindings
[[bindings]]
keysym = "Return"
modifiers = "super"
action = "spawn_terminal"

[[bindings]]
keysym = "d"
modifiers = "super"
action = "spawn_launcher"

# ACPI Lid and display profile rules
[lid]
enable = true
close_profile = "office_clamshell"
open_profile = "office"
undocked_profile = "undocked"
internal_output = "eDP-1"
```
