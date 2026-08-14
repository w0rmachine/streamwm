//! Status/control protocol over a Unix socket (JSON, newline-delimited).
//!
//! streamwm is a Wayland *client* (river is the display server), so it cannot
//! advertise Wayland globals of its own. Instead it exposes a small JSON
//! protocol on `$XDG_RUNTIME_DIR/streamwm-<display>.sock`:
//!
//!   get_status                      -> JSON status snapshot (one line)
//!   { "cmd": "focus_tag", "tag": N }
//!   { "cmd": "send_to_tag", "tag": N }
//!   { "cmd": "focus_output", "output": "eDP-1" }
//!   { "cmd": "spawn", "command": "..." }
//!   { "cmd": "quit" }
//!
//! The socket thread owns a `StatusSnapshot` (built by the event loop) and a
//! `mpsc::Sender<Command>` back to the event loop.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;

use serde::{Deserialize, Serialize};

use crate::state::State;

/// A serializable snapshot of the WM state, Send + Sync.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StatusSnapshot {
    pub focused_output: Option<String>,
    pub outputs: Vec<OutputSnap>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OutputSnap {
    pub name: String,
    pub focused: bool,
    pub active_mask: u32,
    pub occupied_mask: u32,
    pub urgent_mask: u32,
    pub tags: Vec<TagSnap>,
    pub windows: Vec<WindowSnap>,
    pub focused_window: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TagSnap {
    pub id: u32,
    pub label: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowSnap {
    pub id: u32,
    pub app_id: Option<String>,
    pub title: Option<String>,
    pub tag: u32,
}

/// A control command sent from the socket thread to the event loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    FocusTag(u32),
    SendToTag(u32),
    FocusOutput(String),
    Spawn(String),
    Quit,
    /// Notify the loop that a command arrived; it will `manage_dirty`.
    Refresh,
}

/// Build a status snapshot from the current state (pure data, no proxies).
pub fn build_snapshot(state: &State, allow_spawn: bool) -> StatusSnapshot {
    let focused_output_idx = state.active_output();
    let mut snapshot = StatusSnapshot::default();

    for (i, output) in state.outputs.iter().enumerate() {
        let name = output.name.clone().unwrap_or_else(|| format!("output-{i}"));

        if Some(i) == focused_output_idx {
            snapshot.focused_output = Some(name.clone());
        }

        let tags = (0..crate::state::NUM_TAGS)
            .map(|t| TagSnap {
                id: t as u32,
                label: output.tag_labels.get(t).and_then(|l| l.clone()),
            })
            .collect();

        let windows = state
            .windows
            .iter()
            .filter(|w| w.output == i)
            .map(|w| WindowSnap {
                id: w.id,
                app_id: w.app_id.clone(),
                title: w.title.clone(),
                tag: w.tag as u32,
            })
            .collect();

        snapshot.outputs.push(OutputSnap {
            name,
            focused: Some(i) == focused_output_idx,
            active_mask: output.active_mask,
            occupied_mask: output.occupied_mask,
            urgent_mask: output.urgent_mask,
            tags,
            windows,
            focused_window: output.focused_window,
        });
    }

    let _ = allow_spawn;
    snapshot
}

/// Start the status/control socket server in a background thread.
///
/// Returns a `Receiver<Command>` the event loop must poll, plus the
/// `Arc<Mutex<StatusSnapshot>>` the loop updates and the socket writes.
pub fn start() -> (mpsc::Receiver<Command>, Arc<Mutex<StatusSnapshot>>) {
    let socket_path = socket_path();
    let snapshot = Arc::new(Mutex::new(StatusSnapshot::default()));
    let (tx, rx) = mpsc::channel::<Command>();

    let snapshot_for_thread = snapshot.clone();
    let tx_for_thread = tx.clone();

    thread::spawn(move || {
        // Remove stale socket if present.
        let _ = std::fs::remove_file(&socket_path);
        let listener = match UnixListener::bind(&socket_path) {
            Ok(l) => l,
            Err(e) => {
                log::error!("failed to bind status socket {socket_path:?}: {e}");
                return;
            }
        };
        log::info!("status socket listening on {socket_path:?}");

        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            let snap = snapshot_for_thread.clone();
            let tx = tx_for_thread.clone();
            thread::spawn(move || handle_client(stream, snap, tx));
        }
    });

    (rx, snapshot)
}

fn socket_path() -> std::path::PathBuf {
    let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_string());
    let display = std::env::var("WAYLAND_DISPLAY").unwrap_or_else(|_| "wayland-0".to_string());
    std::path::PathBuf::from(runtime).join(format!("streamwm-{display}.sock"))
}

fn handle_client(
    stream: UnixStream,
    snapshot: Arc<Mutex<StatusSnapshot>>,
    tx: mpsc::Sender<Command>,
) {
    let reader = BufReader::new(stream.try_clone().unwrap_or_else(|_| unreachable!()));
    let mut writer = stream;

    for line in reader.lines() {
        let Ok(line) = line else { break };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        // `get_status` is the only non-JSON command (bare word).
        if line == "get_status" {
            let snap = snapshot.lock().unwrap().clone();
            let json = serde_json::to_string(&snap).unwrap_or_else(|_| "{}".into());
            let _ = writeln!(writer, "{json}");
            continue;
        }

        // JSON control command.
        let Ok(cmd) = serde_json::from_str::<serde_json::Value>(line) else {
            let _ = writeln!(writer, "{{\"error\":\"bad json\"}}");
            continue;
        };
        let name = cmd.get("cmd").and_then(|v| v.as_str()).unwrap_or("");
        let result = parse_command_value(&cmd);

        match result {
            Some(cmd) => {
                let _ = tx.send(cmd);
                let _ = writeln!(writer, "{{\"status\":\"ok\"}}");
            }
            None => {
                let _ = writeln!(writer, "{{\"error\":\"unknown command: {name}\"}}");
            }
        }
    }
}

fn parse_command_value(cmd: &serde_json::Value) -> Option<Command> {
    match cmd.get("cmd").and_then(|v| v.as_str()).unwrap_or("") {
        "focus_tag" => cmd
            .get("tag")
            .and_then(|v| v.as_u64())
            .map(|t| Command::FocusTag(t as u32)),
        "send_to_tag" => cmd
            .get("tag")
            .and_then(|v| v.as_u64())
            .map(|t| Command::SendToTag(t as u32)),
        "focus_output" => cmd
            .get("output")
            .and_then(|v| v.as_str())
            .map(|o| Command::FocusOutput(o.to_string())),
        "spawn" => cmd
            .get("command")
            .and_then(|v| v.as_str())
            .map(|c| Command::Spawn(c.to_string())),
        "quit" => Some(Command::Quit),
        _ => None,
    }
}

/// Apply a control command received over the socket to the running WM.
pub fn apply_command(data: &mut crate::connection::AppData, cmd: Command) {
    match cmd {
        Command::FocusTag(tag) => {
            let mut s = data.state.borrow_mut();
            if let Some(o) = s.active_output() {
                s.focus_tag(o, tag as usize);
            }
        }
        Command::SendToTag(tag) => {
            let mut s = data.state.borrow_mut();
            if let Some(o) = s.active_output() {
                s.send_focused_to_tag(o, tag as usize);
            }
        }
        Command::FocusOutput(name) => {
            let mut s = data.state.borrow_mut();
            if let Some(idx) = s.find_output_by_name(&name) {
                // Focus the output (seat focus), and bring its active tag's
                // focused window into focus.
                s.focused_output = Some(idx);
                let fid = s.outputs[idx].focused_window;
                if let Some(fid) = fid {
                    if let Some(w) = s.find_window(fid) {
                        let tag = w.tag;
                        // Ensure the tag is active.
                        s.outputs[idx].active_mask = 1u32 << tag;
                    }
                }
            }
        }
        Command::Spawn(cmd_str) => {
            if data.config.allow_spawn {
                crate::wm::spawn::spawn(&cmd_str);
            }
        }
        Command::Quit => {
            data.quit = true;
        }
        Command::Refresh => {}
    }

    // Trigger a manage sequence so the changes take effect.
    if let Some(wm) = &data.wm {
        wm.manage_dirty();
    }
}

/// Refresh the status snapshot from the current state.
pub fn refresh_snapshot(data: &crate::connection::AppData) {
    if let Some(snapshot) = &data.snapshot {
        let state = data.state.borrow();
        let snap = build_snapshot(&state, data.config.allow_spawn);
        if let Ok(mut guard) = snapshot.lock() {
            *guard = snap;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_control_commands() {
        assert_eq!(
            parse_command_value(&json!({ "cmd": "focus_tag", "tag": 4 })),
            Some(Command::FocusTag(4))
        );
        assert_eq!(
            parse_command_value(&json!({ "cmd": "send_to_tag", "tag": 8 })),
            Some(Command::SendToTag(8))
        );
        assert_eq!(
            parse_command_value(&json!({ "cmd": "focus_output", "output": "eDP-1" })),
            Some(Command::FocusOutput("eDP-1".into()))
        );
        assert_eq!(
            parse_command_value(&json!({ "cmd": "spawn", "command": "foot" })),
            Some(Command::Spawn("foot".into()))
        );
        assert_eq!(
            parse_command_value(&json!({ "cmd": "quit" })),
            Some(Command::Quit)
        );
    }

    #[test]
    fn rejects_missing_or_wrong_command_args() {
        assert_eq!(parse_command_value(&json!({ "cmd": "focus_tag" })), None);
        assert_eq!(
            parse_command_value(&json!({ "cmd": "focus_tag", "tag": "4" })),
            None
        );
        assert_eq!(parse_command_value(&json!({ "cmd": "focus_output" })), None);
        assert_eq!(parse_command_value(&json!({ "cmd": "unknown" })), None);
    }
}
