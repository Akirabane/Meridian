//! The player information forwarding that a Horizon server behind a proxy reads,
//! as Horizon's `docs/velocity-forwarding.md` describes the exchange.
//!
//! The server asks on [`CHANNEL`] during login. The answer is a 32-byte
//! HMAC-SHA256 signature of the data, made with the secret proxy and server share,
//! followed by the data itself: `version` (VarInt), the player's `address`
//! (string), `uuid` (16 bytes), `name` (string), and the profile `properties`
//! (VarInt count; each a name, a value, a bool, and the signature string when the
//! bool is set).
use crate::Profile;
use horizon_protocol::buf::Writer;
use meridian_hmac::hmac_sha256;
use std::net::IpAddr;

/// The login plugin channel the server asks on.
pub const CHANNEL: &str = "velocity:player_info";

/// The only version written: the one Horizon asks for and accepts. A proxy of
/// this kind was recorded answering it to requests for versions 1, 2 and 3 and to a
/// request with no version, so it is what every such server can read.
const VERSION: i32 = 1;

/// The answer to the server's request: the signature, then the signed data.
pub fn answer(secret: &[u8], profile: &Profile, address: IpAddr) -> Vec<u8> {
    let data = data(profile, address);
    let mut out = hmac_sha256(secret, &data).to_vec();
    out.extend_from_slice(&data);
    out
}

/// The signed part of the answer.
fn data(profile: &Profile, address: IpAddr) -> Vec<u8> {
    let mut w = Writer::new();
    w.varint(VERSION);
    w.string(&address.to_string());
    w.uuid(u128::from_be_bytes(profile.uuid));
    w.string(&profile.name);
    w.varint(profile.properties.len() as i32);
    for (name, value, signature) in &profile.properties {
        w.string(name);
        w.string(value);
        w.bool(!signature.is_empty());
        if !signature.is_empty() {
            w.string(signature);
        }
    }
    w.data
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    /// The answer a real proxy of this kind gave for a player named Tester, from
    /// 127.0.0.1, with the throwaway secret below: recorded by Horizon with a
    /// stand-in backend (`docs/velocity-forwarding.md`, the test in
    /// `crates/horizon-client/src/lan/forwarding.rs`).
    const SECRET: &[u8] = b"ISWncFxltwFE";
    const RECORDED: &str = "189e73f57e7fe3d387de5f252fc0226e8288e5b15f23ee12924b34cf216b7f6901093132372e302e302e31f3d28cb072253cb1baeb2dadd2be89ae0654657374657200";

    #[test]
    fn the_answer_matches_a_recorded_one_byte_for_byte() {
        let profile = Profile {
            name: "Tester".into(),
            uuid: 0xf3d28cb072253cb1baeb2dadd2be89ae_u128.to_be_bytes(),
            properties: Vec::new(),
        };
        let answer = answer(SECRET, &profile, "127.0.0.1".parse().unwrap());
        assert_eq!(answer, hex(RECORDED));
    }

    #[test]
    fn another_secret_gives_another_signature_over_the_same_data() {
        let profile = Profile {
            name: "Tester".into(),
            uuid: [7; 16],
            properties: Vec::new(),
        };
        let address = "127.0.0.1".parse().unwrap();
        let (a, b) = (
            answer(SECRET, &profile, address),
            answer(b"another", &profile, address),
        );
        assert_ne!(a[..32], b[..32]);
        assert_eq!(a[32..], b[32..]);
    }

    /// RFC 4231, test cases 1 and 6 (a key longer than the block is hashed first).
    #[test]
    fn hmac_matches_the_rfc_vectors() {
        assert_eq!(
            hmac_sha256(&[0x0b; 20], b"Hi There").to_vec(),
            hex("b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7")
        );
        assert_eq!(
            hmac_sha256(
                &[0xaa; 131],
                b"Test Using Larger Than Block-Size Key - Hash Key First"
            )
            .to_vec(),
            hex("60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54")
        );
    }
}
