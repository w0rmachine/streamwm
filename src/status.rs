//! Unix domain IPC control and status socket server (`$XDG_RUNTIME_DIR/streamwm-<display>.sock`).
//!
//! Exposes a request/response line-delimited JSON interface allowing external tools, bar applications,
//! and scripts to retrieve window manager status snapshots (`get_status`) and send control commands
//! (`focus_tag`, `send_to_tag`, `focus_output`, `focus_window`, `spawn`, `quit`).
//!
//! `subscribe` opts a connection into a long-lived push stream: the current snapshot is sent
//! immediately, then a fresh snapshot is streamed whenever WM state changes (driven from
//! [`refresh_snapshot`]). Delivery is converging — a slow client skips intermediate states and
//! always receives the newest one — so publishing never blocks the WM thread.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;

use serde::{Deserialize, Serialize};

use crate::state::State;

/// Per-subscriber delivery channel depth.
///
/// Capacity is intentionally 1 so delivery is *converging*: a subscriber that
/// cannot keep up never accumulates a backlog — the pending snapshot is simply
/// replaced by the newer one. This keeps memory bounded no matter how slow or
/// wedged a client is.
const SUBSCRIBER_QUEUE_DEPTH: usize = 1;

/// Handle to one connected `subscribe` client.
///
/// The socket thread owns the client stream; the WM thread only ever stores the
/// latest snapshot and signals the client thread, so the WM thread never blocks
/// on client I/O.
struct Subscriber {
    /// Monotonic id used for bookkeeping and log messages.
    id: u64,
    /// Newest snapshot awaiting delivery. Overwritten by later publishes so a
    /// slow client always converges to the most recent state and never builds
    /// a backlog.
    latest: Arc<Mutex<Option<String>>>,
    /// One-byte signal prodding the client thread to flush `latest`. Capacity 1
    /// so at most one wakeup is ever outstanding per subscriber.
    wake: mpsc::SyncSender<()>,
}

/// Registry of active `subscribe` clients, shared between the socket threads and
/// the WM thread that publishes snapshots.
#[derive(Default)]
pub struct Subscribers {
    next_id: u64,
    clients: Vec<Subscriber>,
}

impl Subscribers {
    /// Register a new subscriber and return its id, its latest-snapshot cell,
    /// and the wake receiver the client thread blocks on.
    fn register(&mut self) -> (u64, Arc<Mutex<Option<String>>>, mpsc::Receiver<()>) {
        let id = self.next_id;
        self.next_id += 1;
        let latest = Arc::new(Mutex::new(None));
        let (wake, rx) = mpsc::sync_channel::<()>(SUBSCRIBER_QUEUE_DEPTH);
        self.clients.push(Subscriber {
            id,
            latest: latest.clone(),
            wake,
        });
        (id, latest, rx)
    }

    /// Remove a subscriber that has disconnected.
    fn unregister(&mut self, id: u64) {
        self.clients.retain(|c| c.id != id);
    }

    /// Publish a snapshot to every subscriber.
    ///
    /// Delivery is best-effort and non-blocking from the WM thread's
    /// perspective. Each subscriber stores only the newest snapshot and is
    /// prodded at most once; if it is still busy the stored value is simply
    /// overwritten, so clients converge on the latest state without queueing.
    /// Subscribers whose client thread has exited are pruned.
    fn publish(&mut self, json: &str) {
        self.clients.retain(|c| {
            if let Ok(mut slot) = c.latest.lock() {
                *slot = Some(json.to_string());
            }
            match c.wake.try_send(()) {
                // Stored a newer snapshot; waking is optional since the client
                // thread will re-read `latest` after its current flush.
                Ok(()) => true,
                Err(mpsc::TrySendError::Full(())) => true,
                // Client thread gone: drop the subscriber.
                Err(mpsc::TrySendError::Disconnected(())) => false,
            }
        });
    }
}

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
///
/// Returns the command receiver, the shared snapshot, and the subscriber
/// registry used to push snapshots to `subscribe` clients.
pub fn start(
    wake: UnixStream,
) -> (
    mpsc::Receiver<Command>,
    Arc<Mutex<StatusSnapshot>>,
    Arc<Mutex<Subscribers>>,
) {
    start_on(socket_path(), wake)
}

/// Like [`start`] but binds to an explicit socket path.
///
/// Split out so tests can bind to a temporary path instead of the global
/// `$XDG_RUNTIME_DIR` socket.
fn start_on(
    socket_path: std::path::PathBuf,
    wake: UnixStream,
) -> (
    mpsc::Receiver<Command>,
    Arc<Mutex<StatusSnapshot>>,
    Arc<Mutex<Subscribers>>,
) {
    let snapshot = Arc::new(Mutex::new(StatusSnapshot::default()));
    let subscribers: Arc<Mutex<Subscribers>> = Arc::new(Mutex::new(Subscribers::default()));
    let (tx, rx) = mpsc::channel::<Command>();

    let snapshot_for_thread = snapshot.clone();
    let subscribers_for_thread = subscribers.clone();
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
            let subs = subscribers_for_thread.clone();
            let tx = tx_for_thread.clone();
            let wake = match wake.try_clone() {
                Ok(wake) => wake,
                Err(e) => {
                    log::warn!("failed to clone status wake socket: {e}");
                    continue;
                }
            };
            thread::spawn(move || handle_client(stream, snap, subs, tx, wake));
        }
    });

    (rx, snapshot, subscribers)
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
    subscribers: Arc<Mutex<Subscribers>>,
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
    // would deadlock the client. The one exception is `subscribe`, which
    // deliberately keeps the connection open and streams snapshots.
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

        // `subscribe` opts in to a long-lived push stream instead of the usual
        // one-shot request/response exchange.
        if name == "subscribe" {
            stream_snapshots(writer, snapshot, subscribers);
            return;
        }

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

/// Serve a `subscribe` client: send the current snapshot immediately, then
/// stream the newest snapshot on every change until the client disconnects.
///
/// Blocks only this client's thread — never the WM thread — on its wake
/// channel. Because only the latest snapshot is retained, a slow client skips
/// intermediate states instead of falling behind.
fn stream_snapshots(
    mut writer: UnixStream,
    snapshot: Arc<Mutex<StatusSnapshot>>,
    subscribers: Arc<Mutex<Subscribers>>,
) {
    // Enlarge the write timeout so a briefly busy client is not dropped while
    // a permanently stalled one is still eventually disconnected.
    let _ = writer.set_write_timeout(Some(std::time::Duration::from_secs(5)));

    let (id, latest, wake_rx) = {
        let mut subs = subscribers.lock().unwrap();
        subs.register()
    };
    log::info!("status subscriber {id} connected");

    // Send the current snapshot first so the client renders immediately rather
    // than waiting for the next state change.
    let initial = snapshot.lock().unwrap().clone();
    if let Ok(json) = serde_json::to_string(&initial) {
        if writeln!(writer, "{json}")
            .and_then(|_| writer.flush())
            .is_err()
        {
            subscribers.lock().unwrap().unregister(id);
            log::info!("status subscriber {id} disconnected before first snapshot");
            return;
        }
    }

    // Each wakeup means a newer snapshot may be waiting; always re-read the
    // latest cell so we converge even if several publishes happened meanwhile.
    while wake_rx.recv().is_ok() {
        let pending = latest.lock().ok().and_then(|mut slot| slot.take());
        let Some(json) = pending else { continue };
        if writeln!(writer, "{json}")
            .and_then(|_| writer.flush())
            .is_err()
        {
            break;
        }
    }

    subscribers.lock().unwrap().unregister(id);
    log::info!("status subscriber {id} disconnected");
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

/// Refresh the status snapshot from the current state and push it to any
/// `subscribe` clients.
///
/// Serialization happens once per refresh and is shared across subscribers.
/// Publishing never blocks: a subscriber that has not drained its single-slot
/// channel is skipped and will receive the next snapshot instead.
pub fn refresh_snapshot(data: &crate::connection::AppData) {
    if let Some(snapshot) = &data.snapshot {
        let state = data.state.borrow();
        let snap = build_snapshot(&state, data.config.allow_spawn);
        drop(state);
        if let Ok(mut guard) = snapshot.lock() {
            *guard = snap;
        }
    }

    if let Some(subscribers) = &data.subscribers {
        // Skip cost entirely when nobody is listening.
        let has_clients = subscribers
            .lock()
            .map(|subs| !subs.clients.is_empty())
            .unwrap_or(false);
        if has_clients {
            let json = data
                .snapshot
                .as_ref()
                .and_then(|s| s.lock().ok())
                .and_then(|guard| serde_json::to_string(&*guard).ok());
            if let Some(json) = json {
                if let Ok(mut subs) = subscribers.lock() {
                    subs.publish(&json);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn subscriber_publish_delivers_latest_snapshot() {
        let mut subs = Subscribers::default();
        let (id, latest, rx) = subs.register();

        subs.publish(r#"{"a":1}"#);
        // Wakeup signalled and the latest cell holds the snapshot.
        assert!(rx.try_recv().is_ok());
        assert_eq!(latest.lock().unwrap().as_deref(), Some(r#"{"a":1}"#));

        subs.unregister(id);
        assert!(subs.clients.is_empty());
    }

    #[test]
    fn subscriber_publish_converges_when_client_is_slow() {
        let mut subs = Subscribers::default();
        let (_id, latest, _rx) = subs.register();

        // Publish several snapshots while the client is busy (not draining).
        subs.publish(r#"{"n":1}"#);
        subs.publish(r#"{"n":2}"#);
        subs.publish(r#"{"n":3}"#);

        // Only the newest snapshot is retained; nothing is queued behind it.
        assert_eq!(latest.lock().unwrap().as_deref(), Some(r#"{"n":3}"#));
        assert_eq!(subs.clients.len(), 1);
    }

    #[test]
    fn subscriber_pruned_when_client_thread_exits() {
        let mut subs = Subscribers::default();
        {
            let (_id, _latest, rx) = subs.register();
            drop(rx); // client thread gone
        }
        subs.publish(r#"{"n":1}"#);
        assert!(subs.clients.is_empty());
    }

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
        s1.set_read_timeout(Some(std::time::Duration::from_millis(50)))
            .unwrap();
        let mut reader = BufReader::new(s1);
        let mut line = String::new();
        let result = reader.read_line(&mut line);
        assert!(result.is_err());
    }

    #[test]
    fn subscribe_streams_initial_and_subsequent_snapshots() {
        // Bind the real server to a temporary socket and drive it over the
        // wire, exercising handle_client + stream_snapshots end to end.
        let dir = std::env::temp_dir().join(format!("streamwm-subscribe-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.sock");
        let _ = std::fs::remove_file(&path);

        let (wake_reader, wake_writer) = UnixStream::pair().unwrap();
        wake_reader.set_nonblocking(true).unwrap();
        let (_rx, snapshot, subscribers) = start_on(path.clone(), wake_writer);

        // Wait for the listener to bind.
        for _ in 0..100 {
            if path.exists() {
                break;
            }
            thread::sleep(std::time::Duration::from_millis(10));
        }

        // Seed a known snapshot before connecting.
        *snapshot.lock().unwrap() = StatusSnapshot {
            focused_output: Some("DP-1".into()),
            outputs: vec![],
        };

        let client = UnixStream::connect(&path).unwrap();
        client
            .set_read_timeout(Some(std::time::Duration::from_secs(2)))
            .unwrap();
        {
            let mut w = client.try_clone().unwrap();
            w.write_all(b"{\"cmd\":\"subscribe\"}\n").unwrap();
        }

        let mut reader = BufReader::new(client.try_clone().unwrap());
        let mut line = String::new();

        // 1. The current snapshot is delivered immediately on subscribe.
        reader.read_line(&mut line).unwrap();
        assert!(line.contains("DP-1"), "initial snapshot missing: {line:?}");

        // 2. A published snapshot is streamed to the connected client.
        {
            let mut subs = subscribers.lock().unwrap();
            subs.publish(r#"{"focused_output":"HDMI-1"}"#);
        }

        line.clear();
        reader.read_line(&mut line).unwrap();
        assert!(line.contains("HDMI-1"), "pushed snapshot missing: {line:?}");

        // 3. Dropping the client prunes the subscriber from the registry.
        drop(reader);
        drop(client);
        let mut pruned = false;
        for _ in 0..100 {
            {
                let mut subs = subscribers.lock().unwrap();
                // Force a publish so the dead-channel branch runs.
                subs.publish("{}");
                pruned = subs.clients.is_empty();
            }
            if pruned {
                break;
            }
            thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(pruned, "subscriber was not pruned after disconnect");

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }
}
