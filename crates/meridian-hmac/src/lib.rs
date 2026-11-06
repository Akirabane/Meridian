//! HMAC-SHA256 (RFC 2104) and the comparison of two tags that does not stop at the first byte
//! that differs.
//!
//! It was written for the player information forwarding of `meridian-net`, and moved here so that
//! the proofs of the extension protocol's handshake (docs/EXTENSIONS.md, section 2.2) are made by
//! the same function on both sides of the connection.
use sha2::{Digest, Sha256};

/// The length of a tag, in bytes.
pub const TAG_LEN: usize = 32;

/// HMAC-SHA256 of `data` under `key`, as RFC 2104 defines it.
pub fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; TAG_LEN] {
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

/// Whether `a` and `b` are the same bytes, in a time that depends on their length and not on
/// where they first differ. Tags are the same length; two of different lengths are different.
pub fn equal_in_constant_time(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut difference = 0u8;
    for (x, y) in a.iter().zip(b) {
        difference |= x ^ y;
    }
    difference == 0
}

/// Whether `tag` is the HMAC-SHA256 of `data` under `key`, compared in constant time.
pub fn verify(key: &[u8], data: &[u8], tag: &[u8]) -> bool {
    equal_in_constant_time(&hmac_sha256(key, data), tag)
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

    /// RFC 4231, section 4: the test cases for SHA-256 (cases 5, a truncated tag, is left out).
    #[test]
    fn it_matches_the_rfc_4231_vectors() {
        let long_key = [0xaa; 131];
        let cases: [(Vec<u8>, Vec<u8>, &str); 6] = [
            (
                vec![0x0b; 20],
                b"Hi There".to_vec(),
                "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7",
            ),
            (
                b"Jefe".to_vec(),
                b"what do ya want for nothing?".to_vec(),
                "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843",
            ),
            (
                vec![0xaa; 20],
                vec![0xdd; 50],
                "773ea91e36800e46854db8ebd09181a72959098b3ef8c122d9635514ced565fe",
            ),
            (
                (1..=25).collect(),
                vec![0xcd; 50],
                "82558a389a443c0ea4cc819899f2083a85f0faa3e578f8077a2e3ff46729665b",
            ),
            (
                long_key.to_vec(),
                b"Test Using Larger Than Block-Size Key - Hash Key First".to_vec(),
                "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54",
            ),
            (
                long_key.to_vec(),
                b"This is a test using a larger than block-size key and a larger than block-size data. The key needs to be hashed before being used by the HMAC algorithm.".to_vec(),
                "9b09ffa71b942fcb27635fbcd5b0e944bfdc63644f0713938a7f51535c3a35e2",
            ),
        ];
        for (key, data, tag) in cases {
            assert_eq!(hmac_sha256(&key, &data).to_vec(), hex(tag));
            assert!(verify(&key, &data, &hex(tag)));
        }
    }

    #[test]
    fn a_tag_that_differs_anywhere_or_in_length_is_refused() {
        let tag = hmac_sha256(b"key", b"data");
        assert!(verify(b"key", b"data", &tag));
        for i in 0..TAG_LEN {
            let mut other = tag;
            other[i] ^= 1;
            assert!(!verify(b"key", b"data", &other), "byte {i}");
        }
        assert!(!verify(b"key", b"data", &tag[..31]));
        let mut longer = tag.to_vec();
        longer.push(0);
        assert!(!verify(b"key", b"data", &longer));
        assert!(!verify(b"key", b"data", &[]));
        assert!(!verify(b"another", b"data", &tag));
        assert!(!verify(b"key", b"other data", &tag));
    }

    #[test]
    fn an_empty_key_and_an_empty_message_have_a_tag() {
        assert_eq!(
            hmac_sha256(b"", b"").to_vec(),
            hex("b613679a0814d9ec772f95d778c35fc5ff1697c493715653c6c712144292c5ad")
        );
    }
}
