//! The `favicon` setting: the picture the server list shows beside the proxy. The file
//! is read and checked when the configuration is loaded, at the start and at each
//! `reload`, and kept as the text the status reply carries.
use crate::ConfigError;
use std::path::{Path, PathBuf};

/// The side of the picture, in pixels. The client draws 64 by 64 and refuses
/// another size.
pub const FAVICON_SIDE: u32 = 64;

/// The largest file accepted, in bytes. The status reply is one string of at most
/// 32 767 characters and base64 makes the file a third longer; this leaves room for the
/// rest of the reply, with its sample of names.
pub const MAX_FAVICON_BYTES: usize = 16 * 1024;

const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
/// The last chunk of every PNG file: length 0, `IEND`, and its checksum.
const PNG_END: [u8; 12] = [0, 0, 0, 0, b'I', b'E', b'N', b'D', 0xAE, 0x42, 0x60, 0x82];

/// The favicon of the proxy.
#[derive(Clone, PartialEq, Eq)]
pub struct Favicon {
    /// The file it was read from, resolved against the folder of the configuration file.
    pub path: PathBuf,
    /// The picture as the status reply carries it: `data:image/png;base64,...`.
    pub data_uri: String,
}

// The picture is not worth printing: the path says which one it is.
impl std::fmt::Debug for Favicon {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Favicon")
            .field("path", &self.path)
            .field("bytes", &self.data_uri.len())
            .finish()
    }
}

impl Favicon {
    /// Reads and checks the PNG file at `path`.
    pub(crate) fn read(path: PathBuf) -> Result<Favicon, ConfigError> {
        let invalid = |reason: String| ConfigError::InvalidFavicon {
            path: path.clone(),
            reason,
        };
        let bytes = std::fs::read(&path).map_err(|e| invalid(format!("it cannot be read: {e}")))?;
        check_png(&bytes).map_err(invalid)?;
        let mut data_uri = String::from("data:image/png;base64,");
        data_uri.push_str(&base64(&bytes));
        Ok(Favicon { path, data_uri })
    }
}

/// A PNG file of the right size: its signature, an `IHDR` chunk first with the
/// side asked, and the `IEND` chunk last, so that a file cut short is caught. The
/// pixels are not decoded.
pub(crate) fn check_png(bytes: &[u8]) -> Result<(), String> {
    if bytes.len() > MAX_FAVICON_BYTES {
        return Err(format!(
            "it is {} bytes; the most is {MAX_FAVICON_BYTES}",
            bytes.len()
        ));
    }
    // Signature, then the first chunk: length (4), type (4), width (4), height (4).
    if bytes.len() < 24 || bytes[..8] != PNG_SIGNATURE {
        return Err("it is not a PNG file".to_owned());
    }
    if bytes[8..12] != 13u32.to_be_bytes() || &bytes[12..16] != b"IHDR" {
        return Err("it is not a PNG file: the header chunk is missing".to_owned());
    }
    let width = u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
    let height = u32::from_be_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);
    if (width, height) != (FAVICON_SIDE, FAVICON_SIDE) {
        return Err(format!(
            "it is {width}x{height} pixels; it must be {FAVICON_SIDE}x{FAVICON_SIDE}"
        ));
    }
    if !bytes.ends_with(&PNG_END) {
        return Err("the PNG file is cut short: it does not end with its IEND chunk".to_owned());
    }
    Ok(())
}

/// `path` as written in the file, taken from `folder` when it is relative.
pub(crate) fn resolve(folder: &Path, path: &Path) -> PathBuf {
    crate::sidecar::resolve(folder, path)
}

/// Standard base64, with padding.
pub(crate) fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        let letter = |shift: u32| ALPHABET[((n >> shift) & 63) as usize] as char;
        out.push(letter(18));
        out.push(letter(12));
        out.push(if chunk.len() > 1 { letter(6) } else { '=' });
        out.push(if chunk.len() > 2 { letter(0) } else { '=' });
    }
    out
}
