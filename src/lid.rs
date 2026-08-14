//! Lid-switch / clamshell handling.
//!
//! Switches kanshi output profiles when the laptop lid opens or closes. The
//! previous implementation listened to logind's `PrepareForSleep` signal, but
//! that only fires when the machine actually suspends — closing the lid in a
//! docked clamshell setup (where `HandleLidSwitchDocked=ignore`) produced no
//! event at all.
//!
//! Instead we poll the ACPI lid state file (`/proc/acpi/button/lid/LID0/state`,
//! with a `LID` fallback) and react to open<->closed _transitions_. This is
//! simple, dependency-free, and works regardless of logind's suspend policy.

use std::thread;
use std::time::Duration;

use crate::config::Lid;

/// Poll interval for the lid state file.
const POLL_INTERVAL: Duration = Duration::from_millis(500);
/// Delay after the lid opens before switching kanshi. The ACPI "open" state
/// fires before the panel's DRM connector comes back up, so switching
/// immediately makes kanshi apply the profile while the panel is still
/// "disconnected" and leave it disabled.
const OPEN_DEBOUNCE: Duration = Duration::from_millis(1500);
/// Additional wait before a retry switch, to catch the race where kanshi
/// applies the open profile before the panel is ready.
const OPEN_RETRY_DELAY: Duration = Duration::from_millis(2000);

/// Spawn a background thread that watches lid open/close transitions and
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
    log::info!("lid listener: polling ACPI lid state");

    // Seed the previous state so we only react to *transitions*, not the
    // initial state at startup.
    let mut prev: Option<bool> = None;

    loop {
        let closed = lid_is_closed()?;

        if let Some(prev) = prev {
            if closed != prev {
                if closed {
                    apply_profile(config, true)?;
                } else {
                    // Reopening: wait for the panel to reconnect, switch, then
                    // retry once to catch the race where kanshi applies the
                    // profile before the panel is ready.
                    thread::sleep(OPEN_DEBOUNCE);
                    apply_profile(config, false)?;
                    thread::sleep(OPEN_RETRY_DELAY);
                    apply_profile(config, false)?;
                }
            }
        }
        prev = Some(closed);

        thread::sleep(POLL_INTERVAL);
    }
}

/// Switch kanshi to the close/open profile for the given lid state.
fn apply_profile(config: &Lid, closed: bool) -> Result<(), Box<dyn std::error::Error>> {
    let profile = if closed {
        &config.close_profile
    } else {
        &config.open_profile
    };

    log::info!("lid state: closed={closed}; switching kanshi to `{profile}`");
    crate::wm::spawn::spawn(&format!("kanshictl switch {profile}"));
    Ok(())
}

/// Read the current lid state from /proc/acpi/button/lid.
fn lid_is_closed() -> Result<bool, Box<dyn std::error::Error>> {
    for path in [
        "/proc/acpi/button/lid/LID0/state",
        "/proc/acpi/button/lid/LID/state",
    ] {
        if let Ok(text) = std::fs::read_to_string(path) {
            let lower = text.to_ascii_lowercase();
            if lower.contains("closed") {
                return Ok(true);
            }
            if lower.contains("open") {
                return Ok(false);
            }
        }
    }
    // If the lid state file is unavailable, treat the lid as open. Returning an
    // error would kill the listener; a missing procfs entry is not fatal.
    Ok(false)
}
