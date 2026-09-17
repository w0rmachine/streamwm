//! Process spawning engine for hotkey actions and application launchers.
//!
//! Intelligently determines whether a command can be executed directly via `execvp`
//! (avoiding subshell overhead) or requires `$SHELL -c` execution due to shell syntax
//! characters (pipes, quotes, environment variables, redirects).
//!
//! Spawns child processes detached with null stdio handles and reaps exited children
//! on dedicated background threads to prevent zombie process leakage.

use std::process::{Command, Stdio};

/// Spawns an arbitrary command string detached from streamwm.
/// Automatically selects direct binary execution for simple commands or `$SHELL -c` for shell strings.
pub fn spawn(command: &str) {
    log::info!("spawn: {command}");
    let started = if let Some(args) = direct_command_args(command) {
        spawn_command(args)
    } else {
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
        spawn_command(vec![shell, "-c".to_string(), command.to_string()])
    };

    match started {
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

fn spawn_command(args: Vec<String>) -> std::io::Result<std::process::Child> {
    let Some((program, rest)) = args.split_first() else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "empty command",
        ));
    };

    Command::new(program)
        .args(rest)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
}

/// Analyzes a command string to check if it can be split into plain argument tokens
/// without shell syntax evaluation. Returns `Some(Vec<String>)` for direct execution, or `None` if shell is needed.
fn direct_command_args(command: &str) -> Option<Vec<String>> {
    let trimmed = command.trim();
    if trimmed.is_empty() || trimmed.bytes().any(shell_syntax_byte) {
        return None;
    }

    Some(
        trimmed
            .split_ascii_whitespace()
            .map(ToString::to_string)
            .collect(),
    )
}

/// Returns true if `byte` is a special shell syntax character (`'"`|&$;<>()$`*?~{}[]=\n\r`).
fn shell_syntax_byte(byte: u8) -> bool {
    matches!(
        byte,
        b'\''
            | b'"'
            | b'\\'
            | b'|'
            | b'&'
            | b';'
            | b'<'
            | b'>'
            | b'('
            | b')'
            | b'$'
            | b'`'
            | b'*'
            | b'?'
            | b'~'
            | b'{'
            | b'}'
            | b'['
            | b']'
            | b'='
            | b'\n'
            | b'\r'
    )
}

/// Spawn the configured terminal.
pub fn terminal(terminal: &str) {
    spawn(terminal);
}

/// Spawn the configured launcher.
pub fn launcher(command: &str) {
    spawn(command);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_command_can_spawn_directly() {
        assert_eq!(
            direct_command_args("alacritty"),
            Some(vec!["alacritty".into()])
        );
        assert_eq!(
            direct_command_args("alacritty --class term"),
            Some(vec!["alacritty".into(), "--class".into(), "term".into()])
        );
    }

    #[test]
    fn shell_syntax_uses_shell() {
        assert_eq!(direct_command_args("alacritty -e 'nvim notes.md'"), None);
        assert_eq!(direct_command_args("notify-send hi && alacritty"), None);
        assert_eq!(direct_command_args("FOO=bar alacritty"), None);
    }
}
