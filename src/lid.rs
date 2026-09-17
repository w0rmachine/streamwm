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
use std::process::Command;
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
/// Fast retries after an output topology change. Dock removal and resume can
/// race river's DRM reprobe, so a single immediate profile switch is not
/// sufficient even though sysfs already reports the panel as connected.
const UNDOCKED_RETRY_DELAYS: [Duration; 2] = [Duration::from_secs(2), Duration::from_secs(5)];

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

/// Polling event loop that tracks ACPI lid state transitions and DRM connector topology changes.
fn run(config: &Lid) -> Result<(), Box<dyn std::error::Error>> {
    log::info!("lid listener: polling ACPI lid state");

    // Keep the previous state to distinguish startup from later transitions.
    // Startup is deliberately handled as a topology event: a session may begin
    // with the lid already closed, in which case waiting for a transition would
    // leave Kanshi on an arbitrary profile indefinitely.
    let mut prev_lid_closed: Option<bool> = None;
    let mut prev_topology: Option<DisplayTopology> = None;
    let mut last_undocked_recovery = Instant::now()
        .checked_sub(UNDOCKED_RECOVERY_INTERVAL)
        .unwrap_or_else(Instant::now);
    let mut undocked_retry: Option<(usize, Instant)> = None;

    loop {
        let closed = lid_is_closed()?;
        let topology = display_topology(&config.internal_output);

        let initial_state = prev_topology.is_none();
        let topology_changed = prev_topology != Some(topology);

        if prev_lid_closed.is_none() {
            apply_current_profile(config, closed, topology, "initial display state")?;
        } else if let Some(prev) = prev_lid_closed {
            if closed != prev {
                if closed {
                    apply_closed_profile(config, topology, "lid closed")?;
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

        if !closed && !initial_state {
            let recovery_due = last_undocked_recovery.elapsed() >= UNDOCKED_RECOVERY_INTERVAL;
            let retry_due = undocked_retry.is_some_and(|(_, due)| Instant::now() >= due);

            if should_recover_undocked(closed, topology, topology_changed, retry_due, recovery_due)
            {
                recover_undocked(config)?;
                let recovered_at = Instant::now();
                last_undocked_recovery = recovered_at;

                undocked_retry = if topology_changed {
                    Some((0, recovered_at + UNDOCKED_RETRY_DELAYS[0]))
                } else if let Some((retry, _)) = undocked_retry {
                    let next = retry + 1;
                    UNDOCKED_RETRY_DELAYS
                        .get(next)
                        .map(|delay| (next, recovered_at + *delay))
                } else {
                    None
                };
            } else if topology_changed && topology.external_connected {
                undocked_retry = None;
                apply_profile(&config.open_profile, "external output connected")?;
            } else if !topology.internal_connected || topology.external_connected {
                undocked_retry = None;
            }
        } else if !initial_state && topology_changed {
            // A dock can be attached or removed while the lid is already
            // closed. Re-evaluate instead of waiting for another lid event.
            apply_closed_profile(config, topology, "closed-lid topology changed")?;
            undocked_retry = None;
        } else {
            undocked_retry = None;
        }

        prev_lid_closed = Some(closed);
        prev_topology = Some(topology);

        thread::sleep(POLL_INTERVAL);
    }
}

/// Apply the profile matching the current lid state and connector topology.
fn apply_current_profile(
    config: &Lid,
    closed: bool,
    topology: DisplayTopology,
    reason: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    if closed {
        apply_closed_profile(config, topology, reason)
    } else if topology.internal_connected && !topology.external_connected {
        recover_undocked(config)
    } else {
        apply_profile(&config.open_profile, reason)
    }
}

/// Apply clamshell only when an external display is available. Keeping the
/// internal panel enabled without one prevents a closed-lid undock from
/// stranding the session on a black screen.
fn apply_closed_profile(
    config: &Lid,
    topology: DisplayTopology,
    reason: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    if clamshell_available(topology) {
        apply_profile(&config.close_profile, reason)
    } else {
        log::info!(
            "lid/output recovery: {reason}; no external output is connected, preserving `{}`",
            config.internal_output
        );
        recover_undocked(config)
    }
}

fn clamshell_available(topology: DisplayTopology) -> bool {
    topology.external_connected
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

/// Re-enable the internal panel via `wlr-randr` and switch to the undocked kanshi profile.
fn recover_undocked(config: &Lid) -> Result<(), Box<dyn std::error::Error>> {
    log::info!(
        "lid/output recovery: internal={} connected, no external outputs; switching kanshi to `{}`",
        config.internal_output,
        config.undocked_profile
    );

    run_command(
        "wlr-randr",
        &["--output", &config.internal_output, "--on", "--preferred"],
    );
    switch_kanshi_profile(&config.undocked_profile);
    Ok(())
}

/// Switch kanshi to a named profile, logging the rationale.
fn apply_profile(profile: &str, reason: &str) -> Result<(), Box<dyn std::error::Error>> {
    log::info!("lid/output recovery: {reason}; switching kanshi to `{profile}`");
    switch_kanshi_profile(profile);
    Ok(())
}

/// Helper predicate determining if undocked panel recovery should trigger.
fn should_recover_undocked(
    closed: bool,
    topology: DisplayTopology,
    topology_changed: bool,
    retry_due: bool,
    recovery_due: bool,
) -> bool {
    !closed
        && topology.internal_connected
        && !topology.external_connected
        && (topology_changed || retry_due || recovery_due)
}

/// Split comma-separated fallback profile list into individual profile candidate names.
fn profile_candidates(profile: &str) -> Vec<&str> {
    profile
        .split(',')
        .map(str::trim)
        .filter(|candidate| !candidate.is_empty())
        .collect()
}

/// Execute `kanshictl switch <profile>` trying each profile candidate until one succeeds.
fn switch_kanshi_profile(profile: &str) -> bool {
    let profiles = profile_candidates(profile);
    if profiles.is_empty() {
        log::warn!("no kanshi profile configured for output recovery");
        return false;
    }

    for profile in profiles {
        if run_command("kanshictl", &["switch", profile]) {
            return true;
        }
    }

    log::warn!("none of the configured kanshi profiles matched: `{profile}`");
    false
}

/// Execute an external CLI command synchronously and return true if exit status was success.
fn run_command(program: &str, args: &[&str]) -> bool {
    match Command::new(program).args(args).output() {
        Ok(output) if output.status.success() => true,
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            log::warn!(
                "output recovery command `{program} {}` failed with {}: {}",
                args.join(" "),
                output.status,
                stderr.trim()
            );
            false
        }
        Err(error) => {
            log::warn!(
                "could not run output recovery command `{program} {}`: {error}",
                args.join(" ")
            );
            false
        }
    }
}

/// Read the current lid state from `/proc/acpi/button/lid/LID0/state` (or `LID/state`).
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

/// State representation of connected display outputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DisplayTopology {
    /// True if internal laptop panel DRM connector is connected.
    internal_connected: bool,
    /// True if at least one external monitor DRM connector is connected.
    external_connected: bool,
}

/// Read DRM connector state from `/sys/class/drm`.
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
    fn parses_kanshi_profile_fallbacks() {
        assert_eq!(
            profile_candidates("office, home ,,"),
            vec!["office", "home"]
        );
    }

    #[test]
    fn recovery_requires_an_open_lid_and_only_the_internal_output() {
        let undocked = DisplayTopology {
            internal_connected: true,
            external_connected: false,
        };
        let docked = DisplayTopology {
            internal_connected: true,
            external_connected: true,
        };

        assert!(should_recover_undocked(false, undocked, true, false, false));
        assert!(should_recover_undocked(false, undocked, false, true, false));
        assert!(!should_recover_undocked(true, undocked, true, true, true));
        assert!(!should_recover_undocked(false, docked, true, true, true));
    }

    #[test]
    fn clamshell_requires_an_external_output() {
        let external = DisplayTopology {
            internal_connected: true,
            external_connected: true,
        };
        let internal_only = DisplayTopology {
            internal_connected: true,
            external_connected: false,
        };

        assert!(clamshell_available(external));
        assert!(!clamshell_available(internal_only));
    }
}
