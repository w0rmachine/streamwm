//! streamwm — a tiling window manager for the river Wayland compositor.
#![allow(dead_code)] // several fields are placeholders for upcoming features

mod bindings;
mod config;
mod connection;
mod events;
mod lid;
mod protocols;
mod state;
mod status;
mod wm;

fn main() {
    env_logger::init();

    let config_path =
        std::env::args().nth(1).unwrap_or_else(|| "/etc/streamwm/config.toml".to_string());
    let config = config::Config::load(&config_path);
    log::info!("streamwm starting ({} tags)", config.num_tags());

    // Start the lid-switch listener in the background.
    lid::spawn(config.lid.clone());

    if let Err(e) = connection::run(&config) {
        log::error!("streamwm exited with error: {e}");
        std::process::exit(1);
    }
}
