//! Lid-switch / clamshell handling via systemd logind.
//!
//! Listens to logind's `PrepareForSleep` signal (and, implicitly, lid-switch
//! inhibition state) to switch kanshi output profiles when the lid opens/closes
//! in clamshell mode. The exact profiles are configurable via `[lid]`.

use std::thread;

use crate::config::Lid;

/// Spawn a background thread that listens for logind lid/sleep events and
/// switches kanshi profiles accordingly.
pub fn spawn(config: Lid) {
    if !config.enable {
        return;
    }
    thread::spawn(move || {
        if let Err(e) = run(&config) {
            log::error!("lid listener error: {e}");
        }
    });
}

fn run(config: &Lid) -> Result<(), Box<dyn std::error::Error>> {
    use zbus::blocking::Connection;

    let conn = Connection::system()?;
    let proxy = zbus::blocking::Proxy::new(
        &conn,
        "org.freedesktop.login1",
        "/org/freedesktop/login1",
        "org.freedesktop.login1.Manager",
    )?;

    // Subscribe to the PrepareForSleep signal (bool: true = about to sleep).
    log::info!("lid listener: watching logind PrepareForSleep");
    let signal = proxy.receive_signal("PrepareForSleep")?;

    for _ in signal {
        handle_lid_event(config)?;
    }

    Ok(())
}

fn handle_lid_event(config: &Lid) -> Result<(), Box<dyn std::error::Error>> {
    // Query current lid state to decide which profile to apply.
    let lid_closed = lid_is_closed()?;

    let profile = if lid_closed {
        &config.close_profile
    } else {
        &config.open_profile
    };

    log::info!("lid state: closed={lid_closed}; switching kanshi to `{profile}`");
    crate::wm::spawn::spawn(&format!("kanshictl switch {profile}"));
    Ok(())
}

/// Read the current lid state from /proc/acpi/button/lid or logind.
fn lid_is_closed() -> Result<bool, Box<dyn std::error::Error>> {
    // Try the procfs ACPI lid state file first (most laptops expose it).
    for path in [
        "/proc/acpi/button/lid/LID0/state",
        "/proc/acpi/button/lid/LID/state",
    ] {
        if let Ok(text) = std::fs::read_to_string(path) {
            if text.to_ascii_lowercase().contains("closed") {
                return Ok(true);
            }
            if text.to_ascii_lowercase().contains("open") {
                return Ok(false);
            }
        }
    }
    // Fallback: assume open.
    Ok(false)
}
