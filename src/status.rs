//! Unix domain IPC control and status socket server (`$XDG_RUNTIME_DIR/streamwm-<display>.sock`).
//!
//! Exposes a request/response line-delimited JSON interface allowing external tools, bar applications,
//! and scripts to retrieve window manager status snapshots (`get_status`) and send control commands
//! (`focus_tag`, `send_to_tag`, `focus_output`, `focus_window`, `spawn`, `quit`).

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;

use serde::{Deserialize, Serialize};

use crate::state::State;

/// Thread-safe status snapshot representation returned to JSON socket clients.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StatusSnapshot {
    /// Name of the currently focused output (e.g. `"eDP-1"`).
    pub focused_output: Option<String>,
    /// Snapshot data for each active display output.
    pub outputs: Vec<OutputSnap>,
}

/// Output snapshot containing tag masks and visible window list.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OutputSnap {
    /// Output connector name (e.g. `"eDP-1"`).
    pub name: String,
    /// True if this output currently holds seat keyboard focus.
    pub focused: bool,
    /// Bitmask of the active (visible) tag on this output (e.g. `1 << tag_id`).
    pub active_mask: u32,
    /// Bitmask of tags owned by this output.
    pub owned_mask: u32,
    /// Bitmask of tags owned by this output that contain at least one window.
    pub occupied_mask: u32,
    /// Bitmask of tags with urgent windows.
    pub urgent_mask: u32,
    /// Tag metadata list.
    pub tags: Vec<TagSnap>,
    /// Window metadata list attached to tags owned by this output.
    pub windows: Vec<WindowSnap>,
    /// Streamwm window ID of the focused window on this output, if any.
    pub focused_window: Option<u32>,
}

/// Individual tag snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TagSnap {
    /// Tag index (`0..=8`).
    pub id: u32,
    /// Optional tag display label.
    pub label: Option<String>,
}

/// Individual window snapshot for status reporting.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowSnap {
    /// Streamwm window identifier.
    pub id: u32,
    /// Application ID (e.g. `"alacritty"`).
    pub app_id: Option<String>,
    /// Window title text.
    pub title: Option<String>,
    /// Global tag index this window is on (`0..=8`).
    pub tag: u32,
}

/// Control commands received over the JSON socket and sent to the main loop via `mpsc::channel`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    FocusTag(u32, Option<String>),
    SendToTag(u32),
    FocusOutput(String),
    FocusWindow(String, Option<String>),
    Spawn(String),
    Quit,
    /// Signal the main loop to perform a manage sequence.
    Refresh,
}

/// Constructs a pure data `StatusSnapshot` from current WM state.
pub fn build_snapshot(state: &State, allow_spawn: bool) -> StatusSnapshot {
    let focused_output_idx = state.active_output();
    let mut snapshot = StatusSnapshot::default();
    let num_outputs = state.outputs.len();

    // Pre-allocate output snapshots.
    let mut output_snaps: Vec<OutputSnap> = (0..num_outputs)
        .map(|i| {
            let output = &state.outputs[i];
            let name = output.name.clone().unwrap_or_else(|| format!("output-{i}"));
            let is_focused = Some(i) == focused_output_idx;
            if is_focused {
                snapshot.focused_output = Some(name.clone());
            }

            let active_mask = 1u32 << output.active_tag;
            let tags = (0..crate::state::NUM_TAGS)
                .map(|t| TagSnap {
                    id: t as u32,
                    label: state.tags.get(t).and_then(|tag| tag.label.clone()),
                })
                .collect();

            OutputSnap {
                name,
                focused: is_focused,
                active_mask,
                owned_mask: 0,
                occupied_mask: 0,
                urgent_mask: 0,
                tags,
                windows: Vec::new(),
                focused_window: output.focused_window,
            }
        })
        .collect();

    // Single pass to set owned masks for outputs.
    for (t, tag) in state.tags.iter().enumerate() {
        if let Some(owner) = tag.output {
            if owner < num_outputs {
                output_snaps[owner].owned_mask |= 1u32 << t;
            }
        }
    }

    // Single pass over state.windows to populate occupied masks and window snapshots.
    for w in state.windows.iter() {
        if let Some(owner) = state.tag_owner(w.tag) {
            if owner < num_outputs {
                output_snaps[owner].occupied_mask |= 1u32 << w.tag;
                output_snaps[owner].windows.push(WindowSnap {
                    id: w.id,
                    app_id: w.app_id.clone(),
                    title: w.title.clone(),
                    tag: w.tag as u32,
                });
            }
        }
    }

    snapshot.outputs = output_snaps;
    let _ = allow_spawn;
    snapshot
}

/// Spawns the IPC socket thread listening on `$XDG_RUNTIME_DIR/streamwm-<display>.sock`.
pub fn start(wake: UnixStream) -> (mpsc::Receiver<Command>, Arc<Mutex<StatusSnapshot>>) {
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
            let wake = match wake.try_clone() {
                Ok(wake) => wake,
                Err(e) => {
                    log::warn!("failed to clone status wake socket: {e}");
                    continue;
                }
            };
            thread::spawn(move || handle_client(stream, snap, tx, wake));
        }
    });

    (rx, snapshot)
}

fn socket_path() -> std::path::PathBuf {
    let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_string());
    let display = std::env::var("WAYLAND_DISPLAY").unwrap_or_else(|_| "wayland-0".to_string());
    std::path::PathBuf::from(runtime).join(format!("streamwm-{display}.sock"))
}

/// Client socket request handler (runs on a dedicated client thread).
fn handle_client(
    stream: UnixStream,
    snapshot: Arc<Mutex<StatusSnapshot>>,
    tx: mpsc::Sender<Command>,
    mut wake: UnixStream,
) {
    // Hardening: set a read timeout (2s) so idle or malicious socket connections do not block indefinitely.
    let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(2)));

    let Ok(stream_read) = stream.try_clone() else {
        log::warn!("failed to clone client socket stream");
        return;
    };
    let reader = BufReader::new(stream_read);
    let mut writer = stream;

    // Request/response: read one line, write one response, then close so the
    // client sees EOF (it reads until EOF). Keeping the connection open here
    // would deadlock the client.
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
            break;
        }

        // JSON control command.
        let Ok(cmd) = serde_json::from_str::<serde_json::Value>(line) else {
            let _ = writeln!(writer, "{{\"error\":\"bad json\"}}");
            break;
        };
        let name = cmd.get("cmd").and_then(|v| v.as_str()).unwrap_or("");
        let result = parse_command_value(&cmd);

        match result {
            Some(cmd) => {
                let _ = tx.send(cmd);
                let _ = wake.write_all(&[1]);
                let _ = writeln!(writer, "{{\"status\":\"ok\"}}");
            }
            None => {
                let _ = writeln!(writer, "{{\"error\":\"unknown command: {name}\"}}");
            }
        }
        break;
    }
}

fn parse_command_value(cmd: &serde_json::Value) -> Option<Command> {
    match cmd.get("cmd").and_then(|v| v.as_str()).unwrap_or("") {
        "focus_tag" => {
            let tag = cmd.get("tag").and_then(|v| v.as_u64())?;
            let output = cmd
                .get("output")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            Some(Command::FocusTag(tag as u32, output))
        }
        "send_to_tag" => cmd
            .get("tag")
            .and_then(|v| v.as_u64())
            .map(|t| Command::SendToTag(t as u32)),
        "focus_output" => cmd
            .get("output")
            .and_then(|v| v.as_str())
            .map(|o| Command::FocusOutput(o.to_string())),
        "focus_window" => {
            let app_id = cmd.get("app_id").and_then(|v| v.as_str())?;
            let title = cmd
                .get("title")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            Some(Command::FocusWindow(app_id.to_string(), title))
        }
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
        Command::FocusTag(tag, output) => {
            let mut s = data.state.borrow_mut();
            let o = match output {
                Some(name) => s.find_output_by_name(&name),
                None => s.active_output(),
            };
            if let Some(o) = o {
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
                s.focused_output = Some(idx);
            }
        }
        Command::FocusWindow(app_id, title) => {
            // Focus a specific window (e.g. selected in the quickshell window
            // picker). Switch focus/output/tag to the window's tag so it is
            // visible, then make it the focused window. This lets floating
            // windows — which the picker lists via foreign-toplevel — be
            // focused through streamwm instead of being overridden by the next
            // manage pass.
            let mut s = data.state.borrow_mut();
            let target = s
                .windows
                .iter()
                .find(|w| {
                    w.app_id.as_deref() == Some(app_id.as_str())
                        && (title.is_none()
                            || title.as_deref().is_none()
                            || w.title.as_deref() == title.as_deref())
                })
                .map(|w| (w.id, w.tag));
            if let Some((wid, tag)) = target {
                if let Some(output) = s.tag_owner(tag) {
                    s.focused_output = Some(output);
                    s.outputs[output].active_tag = tag;
                    s.outputs[output].focused_window = Some(wid);
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
            Some(Command::FocusTag(4, None))
        );
        assert_eq!(
            parse_command_value(&json!({ "cmd": "focus_tag", "tag": 4, "output": "eDP-1" })),
            Some(Command::FocusTag(4, Some("eDP-1".into())))
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
            parse_command_value(&json!({ "cmd": "focus_window", "app_id": "foot" })),
            Some(Command::FocusWindow("foot".into(), None))
        );
        assert_eq!(
            parse_command_value(&json!({
                "cmd": "focus_window",
                "app_id": "foot",
                "title": "Term"
            })),
            Some(Command::FocusWindow("foot".into(), Some("Term".into())))
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

    #[test]
    fn build_snapshot_single_pass_occupied_mask() {
        let mut state = State::new(0.55);
        state.tags[0].output = Some(0);
        state.tags[1].output = Some(0);

        let snap = build_snapshot(&state, false);
        assert_eq!(snap.outputs.len(), 0);
    }

    #[test]
    fn socket_read_timeout_prevents_indefinite_blocking() {
        let (s1, _s2) = UnixStream::pair().unwrap();
        s1.set_read_timeout(Some(std::time::Duration::from_millis(50))).unwrap();
        let mut reader = BufReader::new(s1);
        let mut line = String::new();
        let result = reader.read_line(&mut line);
        assert!(result.is_err());
    }
}
