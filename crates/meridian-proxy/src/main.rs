//! The Meridian proxy.
mod args;
mod commands;
mod console;
mod health;
mod log;
// Used by the session once routing is wired in; until then only its tests call it.
#[cfg_attr(not(test), allow(dead_code))]
mod route;
mod serve;
mod session;
mod state;
mod switching;
mod throttle;

use args::Command;
use log::{error, info, warning};
use meridian_config::{Config, ConfigError};
use state::Proxy;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

fn main() -> ExitCode {
    log::init_from_env();
    let command = match args::parse(std::env::args_os().skip(1)) {
        Ok(command) => command,
        Err(message) => {
            eprintln!("meridian: {message}\n\n{}", args::USAGE);
            return ExitCode::from(2);
        }
    };
    match command {
        Command::Help => {
            println!("{}", args::USAGE);
            ExitCode::SUCCESS
        }
        Command::Version => {
            println!("meridian {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Command::Check { config } => match load(&config) {
            Some(loaded) => {
                println!(
                    "{}: valid, {}, default backend \"{}\"",
                    config.display(),
                    backend_count(loaded.backends.len()),
                    loaded.default_backend
                );
                ExitCode::SUCCESS
            }
            None => ExitCode::FAILURE,
        },
        Command::Run { config: path } => match load(&path) {
            Some(loaded) => run(loaded, path),
            None => ExitCode::FAILURE,
        },
    }
}

/// Loads the configuration, or logs why it cannot be used.
fn load(path: &Path) -> Option<Config> {
    match Config::load(path) {
        Ok(config) => Some(config),
        // A read error already names the file.
        Err(e @ ConfigError::Read { .. }) => {
            error!("{e}");
            None
        }
        Err(e) => {
            error!("{}: {e}", path.display());
            None
        }
    }
}

fn run(config: Config, path: PathBuf) -> ExitCode {
    let stop = Arc::new(AtomicBool::new(false));
    {
        let stop = Arc::clone(&stop);
        if let Err(e) = ctrlc::set_handler(move || stop.store(true, Ordering::Relaxed)) {
            error!("cannot watch for Ctrl+C: {e}");
            return ExitCode::FAILURE;
        }
    }
    let listener = match serve::listen(config.listen) {
        Ok(listener) => listener,
        Err(e) => {
            error!("cannot listen on {}: {e}", config.listen);
            return ExitCode::FAILURE;
        }
    };
    info!(
        "meridian {} listening on {}, {}, default backend \"{}\"",
        env!("CARGO_PKG_VERSION"),
        config.listen,
        backend_count(config.backends.len()),
        config.default_backend
    );
    let proxy = Arc::new(Proxy::new(config, path, stop));
    // Without a console the proxy still runs: it then stops on Ctrl+C alone.
    if let Err(e) = console::start(Arc::clone(&proxy)) {
        warning!("cannot start the console: {e}");
    }
    match serve::serve(listener, &proxy) {
        Ok(()) => {
            info!("stopped");
            ExitCode::SUCCESS
        }
        Err(e) => {
            error!("the proxy stopped: {e}");
            ExitCode::FAILURE
        }
    }
}

/// "1 backend", "2 backends".
fn backend_count(n: usize) -> String {
    if n == 1 {
        "1 backend".to_owned()
    } else {
        format!("{n} backends")
    }
}
