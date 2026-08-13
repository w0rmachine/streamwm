//! Wayland connection and the river window-management event loop.

use std::cell::RefCell;
use std::rc::Rc;

use log::info;
use wayland_client::{
    delegate_noop, globals::registry_queue_init,
    protocol::{wl_output::WlOutput, wl_registry, wl_seat::WlSeat, wl_surface::WlSurface},
    Connection as WlConnection, Dispatch, QueueHandle,
};

use crate::config::Config;
use crate::protocols::wm::{
    river_node_v1::RiverNodeV1,
    river_window_manager_v1::{Event as WmEvent, RiverWindowManagerV1},
};
use crate::protocols::xkb_bindings::river_xkb_bindings_v1::RiverXkbBindingsV1;
use crate::state::{Output, Seat, State, Window};

/// Dispatch user-data. Single-threaded; owns a shared handle to the data model.
pub struct AppData {
    /// The window manager data model.
    pub state: Rc<RefCell<State>>,
    /// Bound river_window_manager_v1.
    pub wm: Option<RiverWindowManagerV1>,
    /// Bound river_xkb_bindings_v1.
    pub xkb: Option<RiverXkbBindingsV1>,
    /// Loaded configuration.
    pub config: Rc<Config>,
    /// Set to true to break the event loop.
    pub quit: bool,
    /// Queue handle, set once after registry init.
    pub qh: Option<QueueHandle<AppData>>,
    /// Active keybindings: (binding proxy, action).
    pub bindings: Vec<(
        crate::protocols::xkb_bindings::river_xkb_binding_v1::RiverXkbBindingV1,
        String,
    )>,
    /// Status snapshot, updated after render, read by the socket thread.
    pub snapshot: Option<std::sync::Arc<std::sync::Mutex<crate::status::StatusSnapshot>>>,
}

impl AppData {
    pub fn new(state: Rc<RefCell<State>>, config: Rc<Config>) -> AppData {
        AppData {
            state,
            wm: None,
            xkb: None,
            config,
            quit: false,
            qh: None,
            bindings: Vec::new(),
            snapshot: None,
        }
    }
}

pub fn run(config: &Config) -> Result<(), String> {
    let conn = wayland_client::Connection::connect_to_env()
        .map_err(|e| format!("connect: {e}"))?;
    let state = Rc::new(RefCell::new(State::new()));
    let mut data = AppData::new(state, Rc::new(config.clone()));

    let (globals, mut event_queue) = registry_queue_init::<AppData>(&conn)
        .map_err(|e| format!("registry: {e}"))?;
    let qh = event_queue.handle();
    data.qh = Some(qh.clone());

    bind_globals(&globals, &qh, &mut data)?;

    // Start the status/control socket server.
    let (command_rx, snapshot) = crate::status::start();
    data.snapshot = Some(snapshot);

    info!("streamwm connected; entering event loop");

    loop {
        // Service pending wayland events (non-blocking).
        event_queue
            .dispatch_pending(&mut data)
            .map_err(|e| format!("dispatch: {e}"))?;

        // Service any control commands from the socket thread.
        if let Ok(cmd) = command_rx.try_recv() {
            crate::status::apply_command(&mut data, cmd);
        }

        if data.quit {
            break;
        }

        // Block for the next wayland event.
        event_queue
            .blocking_dispatch(&mut data)
            .map_err(|e| format!("dispatch: {e}"))?;
    }

    info!("streamwm exiting");
    Ok(())
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
                data.state.borrow_mut().windows.push(Window::new(id));
            }
            WmEvent::Output { id } => {
                let mut s = data.state.borrow_mut();
                s.outputs.push(Output::new(id));
                if s.focused_output.is_none() {
                    s.focused_output = Some(s.outputs.len() - 1);
                }
            }
            WmEvent::Seat { id } => {
                let seat_proxy = id.clone();
                data.state.borrow_mut().seats.push(Seat::new(id));
                // Bind keybindings for this seat now that it exists.
                if let Some(xkb) = data.xkb.clone() {
                    crate::bindings::bind_for_seat(data, &xkb, &seat_proxy);
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
delegate_noop!(AppData: ignore WlOutput);
delegate_noop!(AppData: ignore WlSeat);
delegate_noop!(AppData: ignore WlSurface);
