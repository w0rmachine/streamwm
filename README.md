# streamwm

A tiling window manager for the [river](https://isaacfreund.com/software/river)
Wayland compositor, written in Rust.

river is non-monolithic: it is a compositor and delegates *all* window
management to an external "window manager" client implementing the
`river-window-management-v1` protocol. `streamwm` is that client — it owns
layout, focus, tags, borders, keybindings, spawn, and (optionally) server-side
decorations.

## Features

- **Tags 0–9** per output, with bitmask semantics (multiple tags visible at
  once) and rename support.
- **Keybindings** via `river-xkb-bindings-v1`, with arbitrary command `spawn`
  actions.
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
