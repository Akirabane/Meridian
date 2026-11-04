//! The `meridian-sidecar` program: see the crate's README.
use meridian_sidecar::endpoint::Endpoint;
use meridian_sidecar::management::{self, Notices};
use meridian_sidecar::options::{self, Command};
use meridian_sidecar::server::{Settings, serve};
use std::net::TcpListener;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

/// How long a proxy connection may stay silent before it is closed.
const IDLE: Duration = Duration::from_secs(300);

fn main() -> ExitCode {
    let options = match options::parse(std::env::args_os().skip(1)) {
        Ok(Command::Run(options)) => options,
        Ok(Command::Help) => {
            println!("{}", options::USAGE);
            return ExitCode::SUCCESS;
        }
        Ok(Command::Version) => {
            println!("meridian-sidecar {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        Err(message) => {
            eprintln!("meridian-sidecar: {message}\n\n{}", options::USAGE);
            return ExitCode::from(2);
        }
    };
    if let Err(message) = options::check_listen(&options) {
        eprintln!("meridian-sidecar: {message}");
        return ExitCode::from(2);
    }
    let file_text = match &options.secret_file {
        Some(path) => match std::fs::read_to_string(path) {
            Ok(text) => Some(text),
            Err(e) => {
                eprintln!("meridian-sidecar: cannot read {}: {e}", path.display());
                return ExitCode::FAILURE;
            }
        },
        None => None,
    };
    let secret = match options::secret(file_text, std::env::var("MERIDIAN_SIDECAR_SECRET").ok()) {
        Ok(secret) => secret,
        Err(message) => {
            eprintln!("meridian-sidecar: {message}");
            return ExitCode::from(2);
        }
    };
    let listener = match TcpListener::bind(options.listen) {
        Ok(listener) => listener,
        Err(e) => {
            eprintln!("meridian-sidecar: cannot listen on {}: {e}", options.listen);
            return ExitCode::FAILURE;
        }
    };
    let endpoint_file = options.endpoint_file.clone();
    let notices = options
        .management_properties
        .as_ref()
        .map(|_| Arc::new(Notices::default()));
    let stop = Arc::new(AtomicBool::new(false));
    if let (Some(path), Some(notices)) = (&options.management_properties, &notices) {
        if let Err(e) = management::watch(path.clone(), Arc::clone(notices), Arc::clone(&stop)) {
            eprintln!("meridian-sidecar: cannot start the management listener: {e}");
            return ExitCode::FAILURE;
        }
    }
    let settings = Arc::new(Settings {
        secret,
        endpoint: Box::new(move || Endpoint::read(&endpoint_file)),
        endpoint_timeout: options.endpoint_timeout,
        idle_timeout: IDLE,
        insecure_remote: options.insecure_remote,
        notices,
    });
    eprintln!(
        "meridian-sidecar {} listening on {}, backend endpoint {}",
        env!("CARGO_PKG_VERSION"),
        options.listen,
        options.endpoint_file.display()
    );
    // The process stops with Ctrl+C or its service manager; its sockets close
    // with it.
    match serve(listener, settings, &stop) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("meridian-sidecar: stopped: {e}");
            ExitCode::FAILURE
        }
    }
}
