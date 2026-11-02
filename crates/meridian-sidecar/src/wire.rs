//! The signed exchange between the proxy and a sidecar.
//!
//! Each connection starts with the sidecar sending a fresh random challenge:
//!
//! ```text
//! {"challenge":"<64 hex digits>"}
//! ```
//!
//! Every request after it, and every reply, is one line:
//!
//! ```text
//! {"seq":N,"body":"<the request or reply, as a JSON string>","mac":"<64 hex digits>"}
//! ```
//!
//! The MAC is HMAC-SHA256 with the shared secret over the challenge, one byte for
//! the direction (`Q` for a request, `R` for a reply), the sequence number as eight
//! big-endian bytes, and the exact bytes of the body. The body travels as a string
//! so that both sides sign the same bytes, whatever a JSON library does with
//! spacing or key order.
//!
//! A request's sequence number must be higher than the last one on the connection,
//! and a reply carries the number of its request. A request recorded on one
//! connection is refused on another, whose challenge is different, and on its own,
//! where its number is used up. Replies are signed too, so nobody between the two
//! can answer in the sidecar's place. What this does not give is secrecy: see the
//! crate's README.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt;

/// The length of a challenge, in bytes.
pub const CHALLENGE_LEN: usize = 32;

/// A challenge: what makes one connection's signatures useless on another.
pub type Challenge = [u8; CHALLENGE_LEN];

/// Which way a signed line goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Request,
    Reply,
}

impl Direction {
    fn byte(self) -> u8 {
        match self {
            Direction::Request => b'Q',
            Direction::Reply => b'R',
        }
    }
}

/// Why a line was refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WireError {
    /// The line is not the JSON this protocol uses.
    Malformed(String),
    /// The MAC does not match: another secret, or a changed line.
    BadMac,
    /// The sequence number is not the one expected: a replay, or lines out of order.
    BadSeq { got: u64, expected: String },
}

impl fmt::Display for WireError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WireError::Malformed(why) => write!(f, "not a line of the sidecar protocol: {why}"),
            WireError::BadMac => f.write_str("the signature does not match the shared secret"),
            WireError::BadSeq { got, expected } => {
                write!(f, "sequence number {got} where {expected} was expected")
            }
        }
    }
}

impl std::error::Error for WireError {}

#[derive(Serialize, Deserialize)]
struct ChallengeLine {
    challenge: String,
}

#[derive(Serialize, Deserialize)]
struct SignedLine {
    seq: u64,
    body: String,
    mac: String,
}

/// A new random challenge.
pub fn new_challenge() -> std::io::Result<Challenge> {
    let mut challenge = [0u8; CHALLENGE_LEN];
    getrandom::fill(&mut challenge).map_err(|e| std::io::Error::other(e.to_string()))?;
    Ok(challenge)
}

/// The first line of a connection, without its newline.
pub fn challenge_line(challenge: &Challenge) -> String {
    serde_json::to_string(&ChallengeLine {
        challenge: hex(challenge),
    })
    .expect("a string field serialises")
}

/// Reads the first line of a connection.
pub fn read_challenge(line: &str) -> Result<Challenge, WireError> {
    let parsed: ChallengeLine =
        serde_json::from_str(line).map_err(|e| WireError::Malformed(e.to_string()))?;
    unhex(&parsed.challenge)
        .and_then(|bytes| Challenge::try_from(bytes).ok())
        .ok_or_else(|| WireError::Malformed("the challenge is not 32 bytes of hex".into()))
}

/// The MAC of one line.
pub fn mac(
    secret: &[u8],
    challenge: &Challenge,
    direction: Direction,
    seq: u64,
    body: &str,
) -> [u8; 32] {
    let mut data = Vec::with_capacity(CHALLENGE_LEN + 9 + body.len());
    data.extend_from_slice(challenge);
    data.push(direction.byte());
    data.extend_from_slice(&seq.to_be_bytes());
    data.extend_from_slice(body.as_bytes());
    hmac_sha256(secret, &data)
}

/// A signed line, without its newline.
pub fn seal(
    secret: &[u8],
    challenge: &Challenge,
    direction: Direction,
    seq: u64,
    body: &str,
) -> String {
    serde_json::to_string(&SignedLine {
        seq,
        body: body.to_owned(),
        mac: hex(&mac(secret, challenge, direction, seq, body)),
    })
    .expect("plain fields serialise")
}

/// Checks a signed line and returns its sequence number and body. The MAC is
/// checked first, in constant time; then the number, which must be `exactly` when
/// given (a reply), or above `after` (a request).
pub fn open(
    secret: &[u8],
    challenge: &Challenge,
    direction: Direction,
    line: &str,
    expected: Expected,
) -> Result<(u64, String), WireError> {
    let parsed: SignedLine =
        serde_json::from_str(line).map_err(|e| WireError::Malformed(e.to_string()))?;
    let given = unhex(&parsed.mac).ok_or(WireError::BadMac)?;
    let wanted = mac(secret, challenge, direction, parsed.seq, &parsed.body);
    if !constant_time_eq(&given, &wanted) {
        return Err(WireError::BadMac);
    }
    match expected {
        Expected::Exactly(n) if parsed.seq != n => Err(WireError::BadSeq {
            got: parsed.seq,
            expected: n.to_string(),
        }),
        Expected::After(n) if parsed.seq <= n => Err(WireError::BadSeq {
            got: parsed.seq,
            expected: format!("a number above {n}"),
        }),
        _ => Ok((parsed.seq, parsed.body)),
    }
}

/// The sequence number a line must carry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Expected {
    /// A reply: the number of its request.
    Exactly(u64),
    /// A request: a number above the last one used on the connection.
    After(u64),
}

/// HMAC-SHA256, as RFC 2104 defines it.
pub fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; 32] {
    const BLOCK: usize = 64;
    let mut padded = [0u8; BLOCK];
    if key.len() > BLOCK {
        padded[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        padded[..key.len()].copy_from_slice(key);
    }
    let inner = Sha256::new()
        .chain_update(padded.map(|b| b ^ 0x36))
        .chain_update(data)
        .finalize();
    Sha256::new()
        .chain_update(padded.map(|b| b ^ 0x5c))
        .chain_update(inner)
        .finalize()
        .into()
}

/// Compares two byte strings without stopping at the first difference, so the
/// time taken says nothing about where they differ.
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Bytes as lower-case hex.
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Hex back to bytes; `None` for anything that is not an even run of hex digits.
pub fn unhex(text: &str) -> Option<Vec<u8>> {
    if text.len() % 2 != 0 || !text.is_ascii() {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &[u8] = b"a secret shared by proxy and sidecar";

    /// RFC 4231, test cases 1, 2 and 6.
    #[test]
    fn hmac_matches_the_rfc_vectors() {
        assert_eq!(
            hex(&hmac_sha256(&[0x0b; 20], b"Hi There")),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
        assert_eq!(
            hex(&hmac_sha256(b"Jefe", b"what do ya want for nothing?")),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
        assert_eq!(
            hex(&hmac_sha256(
                &[0xaa; 131],
                b"Test Using Larger Than Block-Size Key - Hash Key First"
            )),
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
        );
    }

    #[test]
    fn a_sealed_line_opens_with_the_same_secret_and_challenge() {
        let challenge = [7; CHALLENGE_LEN];
        let line = seal(
            SECRET,
            &challenge,
            Direction::Request,
            3,
            r#"{"op":"drop"}"#,
        );
        assert_eq!(
            open(
                SECRET,
                &challenge,
                Direction::Request,
                &line,
                Expected::After(2)
            ),
            Ok((3, r#"{"op":"drop"}"#.to_owned()))
        );
    }

    #[test]
    fn another_secret_challenge_or_direction_is_refused() {
        let challenge = [7; CHALLENGE_LEN];
        let line = seal(SECRET, &challenge, Direction::Request, 1, "{}");
        let after = Expected::After(0);
        assert_eq!(
            open(
                b"another secret",
                &challenge,
                Direction::Request,
                &line,
                after
            ),
            Err(WireError::BadMac)
        );
        assert_eq!(
            open(
                SECRET,
                &[8; CHALLENGE_LEN],
                Direction::Request,
                &line,
                after
            ),
            Err(WireError::BadMac),
            "a line recorded on another connection"
        );
        assert_eq!(
            open(
                SECRET,
                &challenge,
                Direction::Reply,
                &line,
                Expected::Exactly(1)
            ),
            Err(WireError::BadMac),
            "a request passed off as a reply"
        );
    }

    #[test]
    fn a_changed_body_is_refused() {
        let challenge = [1; CHALLENGE_LEN];
        let line = seal(SECRET, &challenge, Direction::Reply, 4, r#"{"ok":true}"#);
        let forged = line.replace("true", "false");
        assert_eq!(
            open(
                SECRET,
                &challenge,
                Direction::Reply,
                &forged,
                Expected::Exactly(4)
            ),
            Err(WireError::BadMac)
        );
    }

    #[test]
    fn a_used_or_wrong_number_is_refused() {
        let challenge = [2; CHALLENGE_LEN];
        let line = seal(SECRET, &challenge, Direction::Request, 5, "{}");
        assert!(matches!(
            open(
                SECRET,
                &challenge,
                Direction::Request,
                &line,
                Expected::After(5)
            ),
            Err(WireError::BadSeq { got: 5, .. })
        ));
        let reply = seal(SECRET, &challenge, Direction::Reply, 5, "{}");
        assert!(matches!(
            open(
                SECRET,
                &challenge,
                Direction::Reply,
                &reply,
                Expected::Exactly(6)
            ),
            Err(WireError::BadSeq { got: 5, .. })
        ));
    }

    #[test]
    fn a_challenge_goes_through_its_line_and_challenges_differ() {
        let challenge = new_challenge().unwrap();
        assert_eq!(read_challenge(&challenge_line(&challenge)), Ok(challenge));
        assert_ne!(new_challenge().unwrap(), challenge);
        assert!(read_challenge(r#"{"challenge":"abcd"}"#).is_err());
        assert!(read_challenge("hello").is_err());
    }

    #[test]
    fn hex_and_comparison() {
        assert_eq!(hex(&[0, 15, 255]), "000fff");
        assert_eq!(unhex("000fff"), Some(vec![0, 15, 255]));
        assert_eq!(unhex("0g"), None);
        assert_eq!(unhex("abc"), None);
        assert!(constant_time_eq(b"same", b"same"));
        assert!(!constant_time_eq(b"same", b"sane"));
        assert!(!constant_time_eq(b"same", b"same!"));
    }
}
