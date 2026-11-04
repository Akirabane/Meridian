//! The sidecar of a backend: where it listens and the file that holds the secret
//! the proxy shares with it. A backend with no sidecar cannot be switched to or
//! from. The secret itself is never in the configuration: only the path of its
//! file, read when it is needed.
use crate::ConfigError;
use std::path::{Path, PathBuf};

/// The shortest secret accepted, in bytes, as the sidecar accepts it.
pub const MIN_SIDECAR_SECRET: usize = 16;

/// A backend's sidecar.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sidecar {
    /// The backend it serves.
    pub backend: String,
    /// Where the sidecar listens, as `host:port`.
    pub address: String,
    /// The file that holds the secret, already resolved against the folder of
    /// the configuration file.
    pub secret_file: PathBuf,
    /// Whether the proxy asks this sidecar for the notices it hears from the backend's
    /// management protocol (`server/stopping`, `server/started`). Off by default: the
    /// sidecar must be set up to listen as well.
    pub notices: bool,
}

impl Sidecar {
    /// The secret, read from its file as the sidecar reads it: the text without
    /// a final line break, at least [`MIN_SIDECAR_SECRET`] bytes. The error names
    /// the file, never its content.
    pub fn read_secret(&self) -> Result<Vec<u8>, String> {
        read_secret(&self.secret_file)
    }
}

/// See [`Sidecar::read_secret`].
pub fn read_secret(path: &Path) -> Result<Vec<u8>, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let secret = text.trim_end_matches(['\r', '\n']);
    if secret.len() < MIN_SIDECAR_SECRET {
        return Err(format!(
            "{} holds {} bytes; a sidecar secret has at least {MIN_SIDECAR_SECRET}",
            path.display(),
            secret.len()
        ));
    }
    Ok(secret.as_bytes().to_vec())
}

/// `path` as written in the file, taken from `base` when it is relative.
pub(crate) fn resolve(base: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_owned()
    } else {
        base.join(path)
    }
}

/// The same file under another spelling counts as the same file.
fn same_file(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// Each sidecar has its own address and its own secret file, and its secret can
/// be read and is long enough.
pub(crate) fn validate(sidecars: &[Sidecar]) -> Result<(), ConfigError> {
    for (i, sidecar) in sidecars.iter().enumerate() {
        crate::check_address(&sidecar.address).map_err(|reason| {
            ConfigError::InvalidSidecarAddress {
                backend: sidecar.backend.clone(),
                address: sidecar.address.clone(),
                reason,
            }
        })?;
        for other in &sidecars[..i] {
            if other.address == sidecar.address {
                return Err(ConfigError::SharedSidecarAddress {
                    first: other.backend.clone(),
                    second: sidecar.backend.clone(),
                    address: sidecar.address.clone(),
                });
            }
            if same_file(&other.secret_file, &sidecar.secret_file) {
                return Err(ConfigError::SharedSidecarSecret {
                    first: other.backend.clone(),
                    second: sidecar.backend.clone(),
                    path: sidecar.secret_file.clone(),
                });
            }
        }
        sidecar
            .read_secret()
            .map_err(|reason| ConfigError::SidecarSecret {
                backend: sidecar.backend.clone(),
                reason,
            })?;
    }
    Ok(())
}
