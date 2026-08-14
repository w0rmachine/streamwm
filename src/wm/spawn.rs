//! Application spawning: run arbitrary commands for keybindings/launcher
//! with the correct environment (WAYLAND_DISPLAY forwarded).

use std::process::{Command, Stdio};

/// Spawn a shell command detached from streamwm.
pub fn spawn(command: &str) {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
    log::info!("spawn: {command}");
    match Command::new(shell)
        .arg("-c")
        .arg(command)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(mut child) => {
            // Reap the child in a background thread so it doesn't linger as a
            // zombie; streamwm stays responsive while the command runs.
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        }
        Err(e) => log::error!("spawn failed for `{command}`: {e}"),
    }
}

/// Spawn the configured terminal.
pub fn terminal(terminal: &str) {
    spawn(terminal);
}

/// Spawn the configured launcher.
pub fn launcher(command: &str) {
    spawn(command);
}
