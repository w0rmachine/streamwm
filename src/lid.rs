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

use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

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
/// Periodic recovery interval while the lid is open and only the internal
/// panel is connected. This covers output-loss races that happen after
/// undocking without another lid transition.
const UNDOCKED_RECOVERY_INTERVAL: Duration = Duration::from_secs(30);

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
    let mut prev_lid_closed: Option<bool> = None;
    let mut prev_topology: Option<DisplayTopology> = None;
    let mut last_undocked_recovery = Instant::now()
        .checked_sub(UNDOCKED_RECOVERY_INTERVAL)
        .unwrap_or_else(Instant::now);

    loop {
        let closed = lid_is_closed()?;
        let topology = display_topology(&config.internal_output);

        if let Some(prev) = prev_lid_closed {
            if closed != prev {
                if closed {
                    apply_profile(&config.close_profile, "lid closed")?;
                } else {
                    // Reopening: wait for the panel to reconnect, switch, then
                    // retry once to catch the race where kanshi applies the
                    // profile before the panel is ready.
                    thread::sleep(OPEN_DEBOUNCE);
                    recover_open_outputs(config, display_topology(&config.internal_output))?;
                    thread::sleep(OPEN_RETRY_DELAY);
                    recover_open_outputs(config, display_topology(&config.internal_output))?;
                    last_undocked_recovery = Instant::now();
                }
            }
        }

        if !closed {
            let topology_changed = prev_topology != Some(topology);
            let recovery_due = last_undocked_recovery.elapsed() >= UNDOCKED_RECOVERY_INTERVAL;

            if topology.internal_connected
                && !topology.external_connected
                && (topology_changed || recovery_due)
            {
                recover_undocked(config)?;
                last_undocked_recovery = Instant::now();
            } else if topology_changed && topology.external_connected {
                apply_profile(&config.open_profile, "external output connected")?;
            }
        }

        prev_lid_closed = Some(closed);
        prev_topology = Some(topology);

        thread::sleep(POLL_INTERVAL);
    }
}

/// Recover outputs for an open lid based on the current connector topology.
fn recover_open_outputs(
    config: &Lid,
    topology: DisplayTopology,
) -> Result<(), Box<dyn std::error::Error>> {
    if topology.internal_connected && !topology.external_connected {
        recover_undocked(config)
    } else {
        apply_profile(&config.open_profile, "lid opened")
    }
}

/// Re-enable the internal panel and switch to the undocked kanshi profile.
fn recover_undocked(config: &Lid) -> Result<(), Box<dyn std::error::Error>> {
    log::info!(
        "lid/output recovery: internal={} connected, no external outputs; switching kanshi to `{}`",
        config.internal_output,
        config.undocked_profile
    );

    run_shell_detached(&format!(
        "if command -v wlr-randr >/dev/null 2>&1; then wlr-randr --output {} --on --preferred || true; fi; \
         if command -v kanshictl >/dev/null 2>&1; then kanshictl switch {} || true; fi",
        sh_quote(&config.internal_output),
        sh_quote(&config.undocked_profile)
    ));
    Ok(())
}

/// Switch kanshi to a named profile.
fn apply_profile(profile: &str, reason: &str) -> Result<(), Box<dyn std::error::Error>> {
    log::info!("lid/output recovery: {reason}; switching kanshi to `{profile}`");
    run_shell_detached(&format!(
        "if command -v kanshictl >/dev/null 2>&1; then kanshictl switch {} || true; fi",
        sh_quote(profile)
    ));
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DisplayTopology {
    internal_connected: bool,
    external_connected: bool,
}

/// Read DRM connector state from sysfs. The kernel may still know the laptop
/// panel is connected even when river/wlroots temporarily removed the output.
fn display_topology(internal_output: &str) -> DisplayTopology {
    let mut topology = DisplayTopology {
        internal_connected: false,
        external_connected: false,
    };

    let Ok(entries) = fs::read_dir("/sys/class/drm") else {
        return topology;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let Some(output_name) = drm_output_name(name) else {
            continue;
        };

        if connector_is_connected(&path) {
            if output_name == internal_output {
                topology.internal_connected = true;
            } else {
                topology.external_connected = true;
            }
        }
    }

    topology
}

fn connector_is_connected(path: &Path) -> bool {
    fs::read_to_string(path.join("status"))
        .map(|status| status.trim() == "connected")
        .unwrap_or(false)
}

fn drm_output_name(card_name: &str) -> Option<&str> {
    let (_, output) = card_name.split_once('-')?;
    Some(output)
}

fn run_shell_detached(command: &str) {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
    match Command::new(shell)
        .arg("-c")
        .arg(command)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(mut child) => {
            thread::spawn(move || {
                let _ = child.wait();
            });
        }
        Err(e) => log::error!("output recovery spawn failed for `{command}`: {e}"),
    }
}

fn sh_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_output_name_from_drm_connector() {
        assert_eq!(drm_output_name("card1-eDP-1"), Some("eDP-1"));
        assert_eq!(drm_output_name("card1-DP-2"), Some("DP-2"));
        assert_eq!(drm_output_name("renderD128"), None);
    }

    #[test]
    fn shell_quotes_single_quotes() {
        assert_eq!(sh_quote("eDP-1"), "'eDP-1'");
        assert_eq!(sh_quote("bad'value"), "'bad'\\''value'");
    }
}
