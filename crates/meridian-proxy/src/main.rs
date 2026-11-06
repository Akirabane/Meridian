//! The Meridian proxy.
mod access;
mod args;
mod balance;
mod commands;
mod console;
mod evacuate;
mod fallback;
mod health;
mod log;
mod metrics;
mod notices;
mod operators;
mod permissions;
mod placing;
mod queue;
mod restart;
// Used by the session once routing is wired in; until then only its tests call it.
#[cfg_attr(not(test), allow(dead_code))]
mod route;
mod serve;
mod session;
#[cfg(unix)]
mod signals;
mod staff;
mod state;
mod status;
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

/// Loads the configuration, or logs why it cannot be used. What is worth knowing of a
/// configuration that can be used is logged as a warning, for `--check` as for a start.
fn load(path: &Path) -> Option<Config> {
    match Config::load(path) {
        Ok(config) => {
            for warning in config.balancing_warnings() {
                warning!("{}: {warning}", path.display());
            }
            Some(config)
        }
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
    // The bans and the whitelist: a file that cannot be read stops the start, since
    // running without the bans it holds would let in who it keeps out.
    let access = match access::read(config.access.file.as_deref()) {
        Ok(access) => access,
        Err(e) => {
            error!("{e}");
            return ExitCode::FAILURE;
        }
    };
    let access_file = config.access.file.clone();
    // The permissions, the same way: a file that cannot be read stops the start.
    let permissions = match permissions::read(config.permissions.file.as_deref()) {
        Ok(permissions) => permissions,
        Err(e) => {
            error!("{e}");
            return ExitCode::FAILURE;
        }
    };
    let permissions_file = config.permissions.file.clone();
    let listener = match serve::listen(config.listen) {
        Ok(listener) => listener,
        Err(e) => {
            error!("cannot listen on {}: {e}", config.listen);
            return ExitCode::FAILURE;
        }
    };
    for warning in switching::startup_warnings(&config) {
        warning!("{warning}");
    }
    let (listen, backends, default_backend) = (
        config.listen,
        backend_count(config.backends.len()),
        config.default_backend.clone(),
    );
    let proxy = Arc::new(Proxy::new(config, path, stop));
    // Installed before the signals are watched and before the line below, so that a SIGHUP
    // never finds the lists missing.
    if let Some(access) = access {
        info!("bans and whitelist: {}", access::summary(&access));
        proxy.sessions.access.install(access, access_file);
    }
    if let Some(permissions) = permissions {
        info!("permissions: {}", permissions::summary(&permissions));
        proxy
            .sessions
            .permissions
            .install(permissions, permissions_file);
    }
    // SIGTERM stops the proxy and SIGHUP reloads it, where the system has them. Set before
    // the line below, which says the proxy is up, so that a signal sent once it is read
    // is not lost.
    #[cfg(unix)]
    if let Err(e) = signals::spawn(&proxy) {
        warning!("cannot watch for SIGTERM and SIGHUP: {e}");
    }
    info!(
        "meridian {} listening on {listen}, {backends}, default backend \"{default_backend}\"",
        env!("CARGO_PKG_VERSION"),
    );
    // Without a console the proxy still runs: it then stops on Ctrl+C alone.
    if let Err(e) = console::start(Arc::clone(&proxy)) {
        warning!("cannot start the console: {e}");
    }
    // Asks the sidecars for the state of the backends while balancing is on.
    if let Err(e) = status::spawn(&proxy) {
        warning!("cannot start the thread that reads the state of the backends: {e}");
    }
    // The page of counters and the health check, when `[metrics]` is on.
    if let Some(Err(e)) = metrics::spawn(&proxy) {
        warning!("cannot serve the metrics: {e}");
    }
    // Hears the backends' own announcements of their stop, for the sidecars set to give them.
    if let Err(e) = notices::spawn(&proxy) {
        warning!("cannot start the thread that reads what the backends announce: {e}");
    }
    // Follows the servers' own bans and whitelists, and tells them the proxy's, when
    // `[access.sync]` turns either on.
    if let Err(e) = access::sync::spawn(&proxy) {
        warning!("cannot start the threads that keep the bans in step with the servers: {e}");
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
