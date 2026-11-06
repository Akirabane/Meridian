//! The proofs of the handshake (docs/EXTENSIONS.md, section 2.2): each side shows the other that it
//! holds the token, and the token itself never crosses the wire.
//!
//! ```text
//! proxy_nonce, extension_nonce   32 random bytes each, hexadecimal on the wire
//! extension's proof = HMAC-SHA256(token, "meridian-extension/extension" | proxy_nonce | extension_nonce)
//! proxy's proof     = HMAC-SHA256(token, "meridian-extension/proxy"     | extension_nonce | proxy_nonce)
//! ```
//!
//! The `|` is a concatenation of **bytes**: the label's text, then the 32 bytes each nonce stands for
//! (not the 64 characters of its hexadecimal), so that the two sides agree whatever case the hexadecimal
//! was written in. The two labels differ and the nonces swap places, so a proof of one side is never the
//! other's, and a proof cannot be played back in the other direction.
//!
//! A proof is checked in constant time ([`meridian_hmac::verify`]). Nothing here logs, formats or keeps
//! the token.

use crate::wire::SUPPORTED;
use meridian_hmac::{TAG_LEN, hmac_sha256, verify};

/// What the extension's proof is labelled with.
pub const EXTENSION_LABEL: &[u8] = b"meridian-extension/extension";
/// What the proxy's proof is labelled with.
pub const PROXY_LABEL: &[u8] = b"meridian-extension/proxy";
/// The bytes in a nonce.
pub const NONCE_LEN: usize = 32;
/// The shortest token that is accepted (section 2.2).
pub const MIN_TOKEN_LEN: usize = 32;

/// The token both sides hold (docs/EXTENSIONS.md, 2.2). It is a type of its own so that it does not
/// reach a log line, an error or a `Debug` output by being formatted: `Debug` hides it and there is
/// no `Display`. Its bytes are read only by the functions of this module that compute a proof.
pub struct Secret(Vec<u8>);

impl Secret {
    /// A token of these bytes, which must be at least [`MIN_TOKEN_LEN`] of them. The error does not
    /// show them.
    pub fn new(bytes: impl Into<Vec<u8>>) -> Result<Self, String> {
        let bytes = bytes.into();
        if bytes.len() < MIN_TOKEN_LEN {
            return Err(format!(
                "the token is shorter than the {MIN_TOKEN_LEN} bytes it must have"
            ));
        }
        Ok(Self(bytes))
    }

    /// A token from the contents of its file: the line ends and spaces an editor leaves after it are
    /// not part of it.
    pub fn from_file_contents(contents: &[u8]) -> Result<Self, String> {
        let end = contents
            .iter()
            .rposition(|b| !b.is_ascii_whitespace())
            .map_or(0, |last| last + 1);
        Self::new(&contents[..end])
    }

    fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(<hidden>)")
    }
}

/// 32 random bytes, which one side of a handshake chooses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Nonce([u8; NONCE_LEN]);

impl Nonce {
    /// A new nonce, from the system's random bytes.
    pub fn random() -> Result<Self, String> {
        let mut bytes = [0u8; NONCE_LEN];
        getrandom::fill(&mut bytes).map_err(|e| format!("no random bytes for a nonce: {e}"))?;
        Ok(Self(bytes))
    }

    pub fn from_bytes(bytes: [u8; NONCE_LEN]) -> Self {
        Self(bytes)
    }

    /// Reads the hexadecimal of the wire: exactly 64 characters.
    pub fn from_hex(text: &str) -> Result<Self, String> {
        let bytes =
            decode_hex::<NONCE_LEN>(text).ok_or("a nonce that is not 64 hexadecimal digits")?;
        Ok(Self(bytes))
    }

    /// The hexadecimal of the wire, in lower case.
    pub fn to_hex(&self) -> String {
        encode_hex(&self.0)
    }
}

/// The proof the extension sends in its answer to the hello.
pub fn extension_proof(
    token: &Secret,
    proxy_nonce: &Nonce,
    extension_nonce: &Nonce,
) -> [u8; TAG_LEN] {
    hmac_sha256(
        token.as_bytes(),
        &message(EXTENSION_LABEL, proxy_nonce, extension_nonce),
    )
}

/// The proof the proxy sends in `meridian:hello/confirm`.
pub fn proxy_proof(token: &Secret, proxy_nonce: &Nonce, extension_nonce: &Nonce) -> [u8; TAG_LEN] {
    hmac_sha256(
        token.as_bytes(),
        &message(PROXY_LABEL, extension_nonce, proxy_nonce),
    )
}

/// The proof the extension sends, as hexadecimal, for the answer's `proof` field.
pub fn extension_proof_hex(token: &Secret, proxy_nonce: &Nonce, extension_nonce: &Nonce) -> String {
    encode_hex(&extension_proof(token, proxy_nonce, extension_nonce))
}

/// The proof the proxy sends, as hexadecimal, for the confirmation's `proof` field.
pub fn proxy_proof_hex(token: &Secret, proxy_nonce: &Nonce, extension_nonce: &Nonce) -> String {
    encode_hex(&proxy_proof(token, proxy_nonce, extension_nonce))
}

/// Whether `proof` (the hexadecimal of the wire) is the extension's proof for these nonces. Anything
/// that is not 64 hexadecimal digits is simply not the proof. This is the proxy's check.
pub fn check_extension_proof(
    token: &Secret,
    proxy_nonce: &Nonce,
    extension_nonce: &Nonce,
    proof: &str,
) -> bool {
    check(
        token,
        &message(EXTENSION_LABEL, proxy_nonce, extension_nonce),
        proof,
    )
}

/// Whether `proof` is the proxy's proof for these nonces. This is the extension's check.
pub fn check_proxy_proof(
    token: &Secret,
    proxy_nonce: &Nonce,
    extension_nonce: &Nonce,
    proof: &str,
) -> bool {
    check(
        token,
        &message(PROXY_LABEL, extension_nonce, proxy_nonce),
        proof,
    )
}

/// The newest version both sides speak, or `None` if there is none (the extension then answers the
/// error `-32600` with `data.supported`). `offered` is what the proxy's hello lists.
pub fn choose_version(offered: &[u32]) -> Option<u32> {
    offered
        .iter()
        .copied()
        .filter(|version| SUPPORTED.contains(version))
        .max()
}

fn message(label: &[u8], first: &Nonce, second: &Nonce) -> Vec<u8> {
    let mut data = Vec::with_capacity(label.len() + 2 * NONCE_LEN);
    data.extend_from_slice(label);
    data.extend_from_slice(&first.0);
    data.extend_from_slice(&second.0);
    data
}

fn check(token: &Secret, data: &[u8], proof: &str) -> bool {
    match decode_hex::<TAG_LEN>(proof) {
        Some(tag) => verify(token.as_bytes(), data, &tag),
        None => false,
    }
}

fn encode_hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut text, b| {
            let _ = write!(text, "{b:02x}");
            text
        })
}

/// `N` bytes from exactly `2 * N` hexadecimal digits, in either case.
fn decode_hex<const N: usize>(text: &str) -> Option<[u8; N]> {
    let digits = text.as_bytes();
    if digits.len() != 2 * N {
        return None;
    }
    let mut bytes = [0u8; N];
    for (byte, pair) in bytes.iter_mut().zip(digits.chunks_exact(2)) {
        *byte = (digit(pair[0])? << 4) | digit(pair[1])?;
    }
    Some(bytes)
}

fn digit(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::{Confirm, HelloAnswer, Message, decode};

    // Computed apart from this code, with Python's `hmac` and `hashlib`:
    //   hmac.new(TOKEN, b"meridian-extension/extension" + PROXY + EXTENSION, "sha256").hexdigest()
    //   hmac.new(TOKEN, b"meridian-extension/proxy" + EXTENSION + PROXY, "sha256").hexdigest()
    const TOKEN: &[u8] = b"0123456789abcdef0123456789abcdef";
    const PROXY: &str = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";
    const EXTENSION: &str = "ffeeddccbbaa99887766554433221100ffeeddccbbaa99887766554433221100";
    const EXTENSION_PROOF: &str =
        "546198eb05cd02f2d5802912b4e6d9a00d173fdff4a1fd1a33434e875571bffd";
    const PROXY_PROOF: &str = "3719587fddecaa8438f006341b2ae8c64e1bd94b1351f929e5da3870f6e1a226";

    fn secret(bytes: &[u8]) -> Secret {
        Secret::new(bytes).unwrap()
    }

    fn nonces() -> (Nonce, Nonce) {
        (
            Nonce::from_hex(PROXY).unwrap(),
            Nonce::from_hex(EXTENSION).unwrap(),
        )
    }

    #[test]
    fn the_proofs_are_the_ones_computed_apart() {
        let (proxy, extension) = nonces();
        assert_eq!(
            extension_proof_hex(&secret(TOKEN), &proxy, &extension),
            EXTENSION_PROOF
        );
        assert_eq!(
            proxy_proof_hex(&secret(TOKEN), &proxy, &extension),
            PROXY_PROOF
        );
    }

    #[test]
    fn the_golden_files_carry_those_proofs() {
        let golden = |name: &str| {
            let path = format!("{}/golden/protocol-0/{name}", env!("CARGO_MANIFEST_DIR"));
            std::fs::read_to_string(path).unwrap()
        };
        let Message::Response { outcome, .. } =
            Message::parse(&golden("hello.answer.json")).unwrap()
        else {
            panic!("not a response");
        };
        let answer: HelloAnswer = decode(outcome.as_ref().ok()).unwrap();
        assert_eq!(answer.extension_nonce, EXTENSION);
        assert_eq!(answer.proof, EXTENSION_PROOF);

        let Message::Request { params, .. } =
            Message::parse(&golden("confirm.request.json")).unwrap()
        else {
            panic!("not a request");
        };
        let confirm: Confirm = decode(params.as_ref()).unwrap();
        assert_eq!(confirm.proof, PROXY_PROOF);

        let (proxy, extension) = nonces();
        assert!(check_extension_proof(
            &secret(TOKEN),
            &proxy,
            &extension,
            &answer.proof
        ));
        assert!(check_proxy_proof(
            &secret(TOKEN),
            &proxy,
            &extension,
            &confirm.proof
        ));
    }

    #[test]
    fn each_side_accepts_the_others_proof_and_nothing_else() {
        let (proxy, extension) = nonces();
        assert!(check_extension_proof(
            &secret(TOKEN),
            &proxy,
            &extension,
            EXTENSION_PROOF
        ));
        assert!(check_proxy_proof(
            &secret(TOKEN),
            &proxy,
            &extension,
            PROXY_PROOF
        ));
        // The hexadecimal in capitals is the same proof.
        assert!(check_proxy_proof(
            &secret(TOKEN),
            &proxy,
            &extension,
            &PROXY_PROOF.to_uppercase()
        ));
        // A proof of one direction is not the other's.
        assert!(!check_extension_proof(
            &secret(TOKEN),
            &proxy,
            &extension,
            PROXY_PROOF
        ));
        assert!(!check_proxy_proof(
            &secret(TOKEN),
            &proxy,
            &extension,
            EXTENSION_PROOF
        ));
    }

    #[test]
    fn a_wrong_token_fails_on_each_side() {
        let (proxy, extension) = nonces();
        let wrong = b"0123456789abcdef0123456789abcdeX";
        // The extension with a wrong token: the proxy refuses its proof.
        let proof = extension_proof_hex(&secret(wrong), &proxy, &extension);
        assert!(!check_extension_proof(
            &secret(TOKEN),
            &proxy,
            &extension,
            &proof
        ));
        // The proxy with a wrong token: the extension refuses its proof.
        let proof = proxy_proof_hex(&secret(wrong), &proxy, &extension);
        assert!(!check_proxy_proof(
            &secret(TOKEN),
            &proxy,
            &extension,
            &proof
        ));
    }

    #[test]
    fn a_proof_for_other_nonces_fails() {
        let (proxy, extension) = nonces();
        let other = Nonce::from_bytes([7; NONCE_LEN]);
        assert!(!check_extension_proof(
            &secret(TOKEN),
            &other,
            &extension,
            EXTENSION_PROOF
        ));
        assert!(!check_extension_proof(
            &secret(TOKEN),
            &proxy,
            &other,
            EXTENSION_PROOF
        ));
        // The nonces swapped are other nonces.
        assert!(!check_extension_proof(
            &secret(TOKEN),
            &extension,
            &proxy,
            EXTENSION_PROOF
        ));
    }

    #[test]
    fn what_is_not_a_proof_is_not_the_proof() {
        let (proxy, extension) = nonces();
        let one_digit_off = format!("{}0", &EXTENSION_PROOF[..63]);
        for bad in [
            "",
            "00",
            &EXTENSION_PROOF[..62],
            &format!("{EXTENSION_PROOF}00"),
            &one_digit_off,
            &format!("{}g", &EXTENSION_PROOF[..63]),
            &format!(" {}", &EXTENSION_PROOF[..63]),
        ] {
            assert!(
                !check_extension_proof(&secret(TOKEN), &proxy, &extension, bad),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn a_nonce_is_32_bytes_of_hexadecimal() {
        let (proxy, _) = nonces();
        assert_eq!(proxy.to_hex(), PROXY);
        assert_eq!(Nonce::from_hex(&PROXY.to_uppercase()), Ok(proxy));
        for bad in [
            "",
            "00",
            &PROXY[..62],
            &format!("{PROXY}0"),
            &"zz".repeat(32),
        ] {
            assert!(Nonce::from_hex(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn nonces_are_random() {
        let (a, b) = (Nonce::random().unwrap(), Nonce::random().unwrap());
        assert_ne!(a, b);
        assert_ne!(a, Nonce::from_bytes([0; NONCE_LEN]));
        assert_eq!(a.to_hex().len(), 2 * NONCE_LEN);
    }

    #[test]
    fn the_newest_version_in_common_is_chosen() {
        assert_eq!(choose_version(&[0]), Some(0));
        assert_eq!(choose_version(&[2, 0, 1]), Some(0));
        assert_eq!(choose_version(&[1, 2]), None);
        assert_eq!(choose_version(&[]), None);
    }

    #[test]
    fn the_token_is_never_shown_and_must_be_long_enough() {
        let token = secret(TOKEN);
        assert_eq!(format!("{token:?}"), "Secret(<hidden>)");
        let shown = format!("{:?}", Some(&token));
        assert!(!shown.contains("0123"), "{shown}");
        // Too short: refused, and the error does not carry the bytes.
        let error = Secret::new(&TOKEN[..31]).unwrap_err();
        assert!(
            error.contains("32 bytes") && !error.contains("0123"),
            "{error}"
        );
        assert!(Secret::new(Vec::new()).is_err());
    }

    #[test]
    fn a_token_file_loses_the_line_end_an_editor_adds() {
        for contents in [
            TOKEN.to_vec(),
            [TOKEN, b"\n"].concat(),
            [TOKEN, b"\r\n\r\n"].concat(),
            [TOKEN, b"  \n"].concat(),
        ] {
            let token = Secret::from_file_contents(&contents).unwrap();
            let (proxy, extension) = nonces();
            assert_eq!(
                extension_proof_hex(&token, &proxy, &extension),
                EXTENSION_PROOF
            );
        }
        // Only the end is trimmed: a token made short by it is refused, and an empty file is.
        assert!(Secret::from_file_contents(&[TOKEN, b"\n"].concat()[1..32]).is_err());
        assert!(Secret::from_file_contents(b"").is_err());
        assert!(Secret::from_file_contents(b"   \n").is_err());
    }
}
