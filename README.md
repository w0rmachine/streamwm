# streamwm

A tiling window manager for the [river](https://isaacfreund.com/software/river)
Wayland compositor, written in Rust.

river is non-monolithic: it is a compositor and delegates *all* window
management to an external "window manager" client implementing the
`river-window-management-v1` protocol. `streamwm` is that client — it owns
layout, focus, tags, borders, keybindings, spawn, and (optionally) server-side
decorations.

## Features

- **Tags 1–9** unified across all outputs: each tag belongs to one output, is
  created on the output where it's first focused, and migrates to the first
  output when its output is removed.
- **Keybindings** via `river-xkb-bindings-v1`, with arbitrary command `spawn`
  actions.
- **Resize mode** (`Mod+r`): adjust the master/stack split with the arrow keys
  (`←`/`→`, or `h`/`l`), `Escape` to leave.
- **Floating windows** (toggle with `Mod+v`): drag with `Mod`+left-button,
  resize with `Mod`+right-button.
- **Lid-switch / clamshell** handling via systemd logind, switching kanshi
  profiles on open/close.
- **Status/control** over a JSON Unix socket
  (`$XDG_RUNTIME_DIR/streamwm-<display>.sock`), exposing focused output,
  active/occupied/urgent tag masks, and windows.

## Architecture

- `src/connection.rs` — Wayland connection, registry, and the
  manage/render double-buffered event loop.
- `src/protocols.rs` — proc-macro-generated bindings for river's protocols
  (via `wayland-scanner`).
- `src/state.rs` — the data model (outputs, seats, windows, tags).
- `src/events.rs` — window/output/seat event dispatch.
- `src/wm/layout.rs` — tiling layout; `src/wm/spawn.rs` — command spawning.
- `src/bindings.rs` — keybindings and actions.
- `src/status.rs` — the JSON socket server.
- `src/lid.rs` — logind lid listener.

Detailed river/Wayland sequence notes live in
[`docs/wayland-compatibility.md`](docs/wayland-compatibility.md).

## Build

```
cargo build --release     # or
nix build .#default
```

## Configuration

streamwm reads a TOML config. The path is taken from the first CLI argument,
defaulting to `/etc/streamwm/config.toml` if none is given. The `dotfiles` repo
generates this config (at `~/.config/streamwm/config.toml`) and the river
`init` script from Nix, and passes the path explicitly. See `src/config.rs`
for the full schema.

To keep certain windows out of the tiling layout (password prompts,
calculators, the small Google Meet in-call window, etc.), list their app ids in
`floating_app_ids`:

```toml
floating_app_ids = [
  "polkit-gnome-authentication-agent-1",
  "org.gnome.Calculator",
  "google-meet",
]
```

The master/stack split and resize-step are also configurable:

```toml
master_fraction = 0.55   # master window width fraction (0.1..=0.9)
resize_step = 0.05       # amount changed per resize-mode arrow key
```
