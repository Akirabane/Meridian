//! The bans and the whitelist kept in step with the servers: what the notifications mean
//! (`events`), what the proxy takes from a server (`hear`), what it tells the servers
//! (`push`), and both over the signed exchange to a sidecar (`wire`).
use meridian_config::{AccessSettings, AccessSync, Backend, Config, Sidecar};
use std::path::PathBuf;

mod events;
mod hear;
mod push;
mod wire;

/// A sidecar entry for `backend`; its address and secret are for tests that connect.
fn sidecar(backend: &str) -> Sidecar {
    Sidecar {
        backend: backend.into(),
        address: "127.0.0.1:1".into(),
        secret_file: PathBuf::from("unused"),
        notices: false,
    }
}

/// A configuration with `backends`, each with a sidecar, and the two directions as given.
fn config(from_servers: bool, to_servers: bool, backends: &[&str]) -> Config {
    Config {
        backends: backends
            .iter()
            .enumerate()
            .map(|(i, name)| Backend {
                name: (*name).into(),
                address: format!("127.0.0.1:{}", 25601 + i),
            })
            .collect(),
        default_backend: backends[0].into(),
        online_mode: false,
        auth_mode: meridian_config::AuthMode::Offline,
        forwarding_secret: "secret".into(),
        sidecars: backends.iter().map(|b| sidecar(b)).collect(),
        access: AccessSettings {
            file: None,
            sync: AccessSync {
                from_servers,
                to_servers,
            },
        },
        ..Config::default()
    }
}
