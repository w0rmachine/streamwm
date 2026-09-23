//! Wayland client connection establishment and main event loop management.
//!
//! Handles `wayland-client` event queue initialization, global interface binding,
//! file descriptor multiplexing via `nix::poll`, IPC wake socket draining, and lifecycle state management.

use std::cell::RefCell;
use std::os::fd::AsFd;
use std::rc::Rc;
use std::sync::Arc;

use log::info;
use nix::poll::{poll, PollFd, PollFlags, PollTimeout};
use nix::sys::eventfd::{EfdFlags, EventFd};
use nix::sys::signal::{SigSet, Signal};
use nix::sys::signalfd::SignalFd;
use nix::sys::wait::{waitpid, WaitPidFlag, WaitStatus};
use wayland_client::{
    delegate_noop,
    globals::registry_queue_init,
    protocol::{wl_registry, wl_seat::WlSeat, wl_surface::WlSurface},
    Connection as WlConnection, Dispatch, QueueHandle,
};

use crate::config::Config;
use crate::protocols::layer_shell::river_layer_shell_v1::RiverLayerShellV1;
use crate::protocols::wm::{
    river_node_v1::RiverNodeV1,
    river_window_manager_v1::{Event as WmEvent, RiverWindowManagerV1},
};
use crate::protocols::xkb_bindings::river_xkb_bindings_v1::RiverXkbBindingsV1;
use crate::state::{Output, Seat, State, Window};

/// Application dispatch context and state container for the single-threaded event loop.
pub struct AppData {
    /// Interior-mutable window manager state model.
    pub state: Rc<RefCell<State>>,
    /// Bound `river_window_manager_v1` global proxy.
    pub wm: Option<RiverWindowManagerV1>,
    /// Bound `river_xkb_bindings_v1` global proxy for keybindings.
    pub xkb: Option<RiverXkbBindingsV1>,
    /// Bound `river_layer_shell_v1` global proxy signaling layer shell compatibility.
    pub layer_shell: Option<RiverLayerShellV1>,
    /// Active runtime configuration reference.
    pub config: Rc<Config>,
    /// Flag signaling main loop termination when true.
    pub quit: bool,
    /// Wayland queue handle used for instantiating new protocol objects.
    pub qh: Option<QueueHandle<AppData>>,
    /// Wayland global registry handle.
    pub registry: Option<wl_registry::WlRegistry>,
    /// Active registered keybinding proxies mapped to their action strings.
    pub bindings: Vec<(
        crate::protocols::xkb_bindings::river_xkb_binding_v1::RiverXkbBindingV1,
        String,
    )>,
    /// Track whether `river_layer_shell_v1.set_default` has been invoked.
    pub layer_default_set: bool,
    /// List of window IDs queued for protocol close requests in the next manage sequence.
    pub pending_close: Vec<u32>,
    /// Shared thread-safe state snapshot accessed by the JSON socket server.
    pub snapshot: Option<std::sync::Arc<std::sync::Mutex<crate::status::StatusSnapshot>>>,
    /// Registry of long-lived `subscribe` clients receiving pushed snapshots.
    pub subscribers: Option<std::sync::Arc<std::sync::Mutex<crate::status::Subscribers>>>,
    /// Active registered pointer binding proxies mapped to `"move"` or `"resize"`.
    pub pointer_bindings: Vec<(
        crate::protocols::wm::river_pointer_binding_v1::RiverPointerBindingV1,
        String,
    )>,
    /// Currently active interactive pointer operation (moving or resizing a floating window).
    pub pointer_op: Option<PointerOp>,
    /// Interactive pointer operation queued to start during the next manage sequence.
    pub pending_op: Option<PointerOp>,
    /// Flag indicating an interactive pointer operation end (`op_end`) should be sent.
    pub op_end_requested: bool,
    /// Target output index for pointer warping following keyboard output/tag switches.
    pub pending_pointer_warp: Option<usize>,
}

/// State of an active interactive pointer operation (drag-to-move or drag-to-resize) for a floating window.
pub struct PointerOp {
    /// Window ID being manipulated.
    pub window: u32,
    /// Type of pointer operation (Move or Resize).
    pub kind: OpKind,
    /// Initial pointer X position (logical pixels) when operation started.
    pub start_x: i32,
    /// Initial pointer Y position (logical pixels) when operation started.
    pub start_y: i32,
    /// Window float X coordinate at operation start.
    pub start_float_x: i32,
    /// Window float Y coordinate at operation start.
    pub start_float_y: i32,
    /// Window float width at operation start.
    pub start_w: u32,
    /// Window float height at operation start.
    pub start_h: u32,
    /// Which edges are being resized (for `OpKind::Resize`); empty for Move.
    pub edges: crate::protocols::wm::river_window_v1::Edges,
    /// Wayland seat proxy controlling the operation.
    pub seat: crate::protocols::wm::river_seat_v1::RiverSeatV1,
}

/// Type of interactive pointer manipulation.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OpKind {
    /// Translating window floating position.
    Move,
    /// Adjusting window floating dimensions.
    Resize,
}

impl AppData {
    /// Constructs a new `AppData` state context with default empty handles.
    pub fn new(state: Rc<RefCell<State>>, config: Rc<Config>) -> AppData {
        AppData {
            state,
            wm: None,
            xkb: None,
            layer_shell: None,
            config,
            quit: false,
            qh: None,
            registry: None,
            bindings: Vec::new(),
            layer_default_set: false,
            pending_close: Vec::new(),
            snapshot: None,
            subscribers: None,
            pointer_bindings: Vec::new(),
            pointer_op: None,
            pending_op: None,
            op_end_requested: false,
            pending_pointer_warp: None,
        }
    }
}

/// Connects to the Wayland display socket, initializes River protocol globals, starts the socket server,
/// and executes the primary event dispatch loop using `nix::poll`.
pub fn run(config: &Config) -> Result<(), String> {
    let conn = wayland_client::Connection::connect_to_env().map_err(|e| format!("connect: {e}"))?;
    let state = Rc::new(RefCell::new(State::new(
        config.master_fraction.clamp(0.1, 0.9),
    )));
    let mut data = AppData::new(state, Rc::new(config.clone()));

    let (globals, mut event_queue) =
        registry_queue_init::<AppData>(&conn).map_err(|e| format!("registry: {e}"))?;
    let qh = event_queue.handle();
    data.qh = Some(qh.clone());
    data.registry = Some(globals.registry().clone());

    bind_globals(&globals, &qh, &mut data)?;

    // Start the status/control socket server. The wake eventfd is signalled
    // by the socket thread when a command arrives; the main loop polls it
    // alongside the Wayland and SIGCHLD fds.
    let wake = Arc::new(
        EventFd::from_flags(EfdFlags::EFD_CLOEXEC | EfdFlags::EFD_NONBLOCK)
            .map_err(|e| format!("status wake eventfd: {e}"))?,
    );
    let (command_rx, snapshot, subscribers) = crate::status::start(wake.clone());
    data.snapshot = Some(snapshot);
    data.subscribers = Some(subscribers);

    // Reap children via SIGCHLD instead of a thread per spawned process.
    let mut sigset = SigSet::empty();
    sigset.add(Signal::SIGCHLD);
    // Block SIGCHLD in this thread so it is delivered to the signalfd rather
    // than interrupting poll; the fd becomes readable when a child exits.
    nix::sys::signal::pthread_sigmask(nix::sys::signal::SigmaskHow::SIG_BLOCK, Some(&sigset), None)
        .map_err(|e| format!("block SIGCHLD: {e}"))?;
    let sigfd = SignalFd::with_flags(&sigset, nix::sys::signalfd::SfdFlags::SFD_NONBLOCK)
        .map_err(|e| format!("signalfd: {e}"))?;

    info!("streamwm connected; entering event loop");

    loop {
        // Service pending wayland events (non-blocking).
        event_queue
            .dispatch_pending(&mut data)
            .map_err(|e| format!("dispatch: {e}"))?;

        service_control_commands(&command_rx, &mut data);

        if data.quit {
            break;
        }

        conn.flush().map_err(|e| format!("flush: {e}"))?;

        let Some(read_guard) = event_queue.prepare_read() else {
            continue;
        };

        let wayland_ready;
        let wake_ready;
        let sig_ready;
        {
            let mut fds = [
                PollFd::new(read_guard.connection_fd(), PollFlags::POLLIN),
                PollFd::new(wake.as_fd(), PollFlags::POLLIN),
                PollFd::new(sigfd.as_fd(), PollFlags::POLLIN),
            ];
            poll(&mut fds, PollTimeout::NONE).map_err(|e| format!("poll: {e}"))?;
            wayland_ready = fds[0]
                .revents()
                .unwrap_or_else(PollFlags::empty)
                .intersects(PollFlags::POLLIN | PollFlags::POLLHUP | PollFlags::POLLERR);
            wake_ready = fds[1]
                .revents()
                .unwrap_or_else(PollFlags::empty)
                .contains(PollFlags::POLLIN);
            sig_ready = fds[2]
                .revents()
                .unwrap_or_else(PollFlags::empty)
                .contains(PollFlags::POLLIN);
        }

        if wayland_ready {
            read_guard.read().map_err(|e| format!("read: {e}"))?;
        }

        if wake_ready {
            while wake.read().is_ok() {}
            service_control_commands(&command_rx, &mut data);
        }

        if sig_ready {
            reap_children(&sigfd);
        }
    }

    info!("streamwm exiting");
    Ok(())
}

/// Drain the SIGCHLD signalfd and reap every exited child with `waitpid`,
/// preventing zombie accumulation without a dedicated thread per spawn.
fn reap_children(sigfd: &SignalFd) {
    while sigfd.read_signal().is_ok() {
        loop {
            match waitpid(None, Some(WaitPidFlag::WNOHANG)) {
                Ok(WaitStatus::StillAlive) => break,
                Err(nix::errno::Errno::ECHILD) => break,
                Err(e) => {
                    log::warn!("reap child failed: {e}");
                    break;
                }
                Ok(_) => {}
            }
        }
    }
}

fn service_control_commands(
    command_rx: &std::sync::mpsc::Receiver<crate::status::Command>,
    data: &mut AppData,
) {
    while let Ok(cmd) = command_rx.try_recv() {
        crate::status::apply_command(data, cmd);
    }
}

fn bind_globals(
    globals: &wayland_client::globals::GlobalList,
    qh: &QueueHandle<AppData>,
    data: &mut AppData,
) -> Result<(), String> {
    let wm = globals
        .bind::<RiverWindowManagerV1, _, ()>(qh, 1..=4, ())
        .map_err(|e| format!("bind river_window_manager_v1: {e}"))?;
    data.wm = Some(wm);

    if let Ok(xkb) = globals.bind::<RiverXkbBindingsV1, _, ()>(qh, 1..=3, ()) {
        data.xkb = Some(xkb);
    }

    // Bind layer-shell; without it the compositor closes all wlr-layer-shell
    // surfaces (quickshell bar, swaybg background, etc.).
    if let Ok(layer) = globals.bind::<RiverLayerShellV1, _, ()>(qh, 1..=1, ()) {
        data.layer_shell = Some(layer);
    } else {
        log::warn!("river_layer_shell_v1 not advertised; layer surfaces unavailable");
    }

    Ok(())
}

impl Dispatch<RiverWindowManagerV1, ()> for AppData {
    fn event(
        data: &mut Self,
        wm: &RiverWindowManagerV1,
        event: WmEvent,
        _ud: &(),
        _conn: &WlConnection,
        _qh: &QueueHandle<Self>,
    ) {
        match event {
            WmEvent::Unavailable => {
                log::error!("window management unavailable (another WM running?)");
                data.quit = true;
            }
            WmEvent::Finished => {
                data.quit = true;
            }
            WmEvent::Window { id } => {
                let mut state = data.state.borrow_mut();
                let output = state.active_output().unwrap_or(0);
                let tag = state.outputs.get(output).map(|o| o.active_tag).unwrap_or(0);
                state.windows.push(Window::new(id, tag));
                if let Some(window) = state.windows.last() {
                    let wid = window.id;
                    if let Some(out) = state.outputs.get_mut(output) {
                        out.focused_window = Some(wid);
                    }
                }
            }
            WmEvent::Output { id } => {
                let mut s = data.state.borrow_mut();
                s.outputs.push(Output::new(id));
                let new_idx = s.outputs.len() - 1;
                s.assign_initial_tag(new_idx);
                if s.focused_output.is_none() {
                    s.focused_output = Some(new_idx);
                }
                // Create layer-shell output state for this output.
                let layer = data.layer_shell.clone();
                let qh = data.qh.clone();
                if let (Some(layer), Some(qh)) = (layer, qh) {
                    if let Some(out) = s.outputs.last_mut() {
                        let proxy = out.proxy.clone();
                        if out.layer.is_none() {
                            let l = layer.get_output(&proxy, &qh, ());
                            out.layer = Some(l);
                        }
                    }
                }
            }
            WmEvent::Seat { id } => {
                let seat_proxy = id.clone();
                data.state.borrow_mut().seats.push(Seat::new(id));
                // Bind keybindings for this seat now that it exists.
                if let Some(xkb) = data.xkb.clone() {
                    crate::bindings::bind_for_seat(data, &xkb, &seat_proxy);
                }
                // Bind pointer bindings (Mod+drag move/resize) for floating
                // windows.
                crate::bindings::bind_pointer_for_seat(data, &seat_proxy);
                // Create layer-shell seat state for this seat.
                let layer = data.layer_shell.clone();
                let qh = data.qh.clone();
                if let (Some(layer), Some(qh)) = (layer, qh) {
                    let mut s = data.state.borrow_mut();
                    if let Some(seat) = s.seats.last_mut() {
                        if seat.layer.is_none() {
                            let l = layer.get_seat(&seat_proxy, &qh, ());
                            seat.layer = Some(l);
                        }
                    }
                }
            }
            WmEvent::ManageStart => {
                let wm = wm.clone();
                crate::wm::on_manage_start(data, &wm);
            }
            WmEvent::RenderStart => {
                let wm = wm.clone();
                crate::wm::on_render_start(data, &wm);
            }
            WmEvent::SessionLocked | WmEvent::SessionUnlocked => {}
        }
    }

    fn event_created_child(
        opcode: u16,
        qhandle: &QueueHandle<Self>,
    ) -> std::sync::Arc<dyn wayland_client::backend::ObjectData> {
        use crate::protocols::wm::river_output_v1::RiverOutputV1;
        use crate::protocols::wm::river_seat_v1::RiverSeatV1;
        use crate::protocols::wm::river_window_manager_v1::{
            EVT_OUTPUT_OPCODE, EVT_SEAT_OPCODE, EVT_WINDOW_OPCODE,
        };
        use crate::protocols::wm::river_window_v1::RiverWindowV1;

        match opcode {
            EVT_WINDOW_OPCODE => qhandle.make_data::<RiverWindowV1, _>(()),
            EVT_OUTPUT_OPCODE => qhandle.make_data::<RiverOutputV1, _>(()),
            EVT_SEAT_OPCODE => qhandle.make_data::<RiverSeatV1, _>(()),
            _ => panic!(
                "Missing event_created_child specialization for event opcode {} of river_window_manager_v1",
                opcode
            ),
        }
    }
}

// Registry: the user-data is GlobalListContents, provided by registry_queue_init.
impl Dispatch<wl_registry::WlRegistry, wayland_client::globals::GlobalListContents> for AppData {
    fn event(
        _data: &mut Self,
        _registry: &wl_registry::WlRegistry,
        _event: wl_registry::Event,
        _ud: &wayland_client::globals::GlobalListContents,
        _conn: &WlConnection,
        _qh: &QueueHandle<Self>,
    ) {
        // Handled internally by registry_queue_init.
    }
}

delegate_noop!(AppData: ignore RiverNodeV1);
delegate_noop!(AppData: ignore RiverXkbBindingsV1);
delegate_noop!(AppData: ignore WlSeat);
delegate_noop!(AppData: ignore WlSurface);
delegate_noop!(AppData: ignore crate::protocols::layer_shell::river_layer_shell_v1::RiverLayerShellV1);
delegate_noop!(AppData: ignore crate::protocols::layer_shell::river_layer_shell_seat_v1::RiverLayerShellSeatV1);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wake_eventfd_is_nonblocking() {
        let wake = EventFd::from_flags(EfdFlags::EFD_NONBLOCK).unwrap();
        // Empty eventfd: a read would block, and with EFD_NONBLOCK it errors
        // instead. Counter stays zero until something is enqueued.
        assert!(wake.read().is_err());
    }
}
