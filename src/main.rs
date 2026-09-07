//! streamwm — a tiling window manager for the river Wayland compositor.
#![allow(dead_code)] // several fields are placeholders for upcoming features

use std::collections::VecDeque;
use std::ffi::OsString;
use std::process::{Command, ExitCode};
use std::thread;
use std::time::{Duration, Instant};

mod bindings;
mod config;
mod connection;
mod events;
mod lid;
mod protocols;
mod state;
mod status;
mod wm;

const WORKER_ARG: &str = "--streamwm-worker";
const RESTART_WINDOW: Duration = Duration::from_secs(60);
const MAX_RESTARTS_IN_WINDOW: usize = 5;
const INITIAL_RESTART_DELAY: Duration = Duration::from_millis(250);
const MAX_RESTART_DELAY: Duration = Duration::from_secs(4);

fn main() -> ExitCode {
    if std::env::var("RUST_LOG").is_err() {
        std::env::set_var("RUST_LOG", "info");
    }
    env_logger::init();

    let mut args = std::env::args_os().skip(1);
    if args.next().as_deref() == Some(std::ffi::OsStr::new(WORKER_ARG)) {
        return run_worker(args.next());
    }

    supervise(std::env::args_os().skip(1).collect())
}

fn run_worker(config_path: Option<OsString>) -> ExitCode {
    let config_path = config_path
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|| "/etc/streamwm/config.toml".to_string());
    let config = config::Config::load(&config_path);
    log::info!("streamwm starting ({} tags)", config.num_tags());

    // Start the lid-switch listener in the background.
    lid::spawn(config.lid.clone());

    if let Err(e) = connection::run(&config) {
        log::error!("streamwm exited with error: {e}");
        return ExitCode::FAILURE;
    }

    ExitCode::SUCCESS
}

fn supervise(args: Vec<OsString>) -> ExitCode {
    let executable = match std::env::current_exe() {
        Ok(path) => path,
        Err(e) => {
            log::error!("failed to resolve streamwm executable: {e}");
            return ExitCode::FAILURE;
        }
    };
    let mut failures = VecDeque::new();

    loop {
        let started = Instant::now();
        let status = Command::new(&executable)
            .arg(WORKER_ARG)
            .args(&args)
            .status();

        match status {
            Ok(status) if status.success() => return ExitCode::SUCCESS,
            Ok(status) => {
                log::warn!(
                    "streamwm worker exited with {status} after {:?}",
                    started.elapsed()
                );
            }
            Err(e) => log::error!("failed to start streamwm worker: {e}"),
        }

        let now = Instant::now();
        while failures
            .front()
            .is_some_and(|failed_at| now.duration_since(*failed_at) >= RESTART_WINDOW)
        {
            failures.pop_front();
        }
        failures.push_back(now);

        if failures.len() >= MAX_RESTARTS_IN_WINDOW {
            log::error!(
                "streamwm worker failed {MAX_RESTARTS_IN_WINDOW} times within {}s; giving up",
                RESTART_WINDOW.as_secs()
            );
            return ExitCode::FAILURE;
        }

        let delay = restart_delay(failures.len());
        log::warn!("restarting streamwm worker in {delay:?}");
        thread::sleep(delay);
    }
}

fn restart_delay(recent_failures: usize) -> Duration {
    let exponent = recent_failures.saturating_sub(1).min(4) as u32;
    INITIAL_RESTART_DELAY
        .saturating_mul(1 << exponent)
        .min(MAX_RESTART_DELAY)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restart_delay_uses_bounded_exponential_backoff() {
        assert_eq!(restart_delay(1), Duration::from_millis(250));
        assert_eq!(restart_delay(2), Duration::from_millis(500));
        assert_eq!(restart_delay(5), Duration::from_secs(4));
        assert_eq!(restart_delay(100), Duration::from_secs(4));
    }
}
