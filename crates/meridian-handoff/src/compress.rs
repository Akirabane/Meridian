//! The compression of the packets towards the player, at the level the configuration chose (`compression-level`).
//!
//! `horizon_protocol::frame::contents` compresses at zlib's default level, 6, and this module calls it at that
//! level: the bytes are its own. At another level it does the same thing with only the level changed: the same
//! backend (flate2 with zlib-rs, the one horizon-protocol pulls), the same zlib header and window, the same length
//! prefix, the same limit.

use flate2::{Compression, write::ZlibEncoder};
use horizon_protocol::frame::MAX_FRAME;
use std::io::Write;

/// What `frame::contents(body, threshold)` gives, a packet at or above the threshold compressed at `level`.
pub(crate) fn contents(
    body: &[u8],
    threshold: Option<usize>,
    level: Compression,
) -> horizon_protocol::Result<Vec<u8>> {
    match threshold {
        Some(t) if body.len() >= t && level != Compression::default() => {
            let mut out = Vec::with_capacity(body.len() / 2 + 8);
            horizon_protocol::buf::write_varint(&mut out, body.len() as i32);
            let mut z = ZlibEncoder::new(out, level);
            z.write_all(body)?;
            let out = z.finish()?;
            if out.len() > MAX_FRAME {
                return Err(horizon_protocol::Error::new(format!(
                    "packet too large: {} bytes",
                    out.len()
                )));
            }
            Ok(out)
        }
        _ => horizon_protocol::frame::contents(body, threshold),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::read::ZlibDecoder;
    use std::io::Read;

    /// Bodies like a server's: random bytes, palette-like runs and zeros, at the sizes around the threshold and up.
    fn corpus() -> Vec<Vec<u8>> {
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let mut out = Vec::new();
        for size in [
            0,
            1,
            255,
            256,
            257,
            1 << 10,
            4 << 10,
            16 << 10,
            30 << 10,
            64 << 10,
        ] {
            let random: Vec<u8> = (0..size).map(|_| next() as u8).collect();
            let mut runs = Vec::with_capacity(size);
            while runs.len() < size {
                let value = (next() % 12) as u8;
                let run = 1 + (next() % 40) as usize;
                runs.extend(std::iter::repeat_n(value, run.min(size - runs.len())));
            }
            out.extend([random, runs, vec![0u8; size]]);
        }
        out
    }

    /// The body a compressed frame holds, read back: its length prefix, then the zlib stream inflated.
    fn inflate(contents: &[u8]) -> Vec<u8> {
        let mut reader = horizon_protocol::buf::Reader::new(contents);
        let len = reader.varint().unwrap() as usize;
        let mut body = Vec::new();
        ZlibDecoder::new(&contents[reader.position()..])
            .read_to_end(&mut body)
            .unwrap();
        assert_eq!(body.len(), len);
        body
    }

    #[test]
    fn the_default_level_gives_the_bytes_of_frame_contents() {
        for threshold in [None, Some(0), Some(256), Some(1 << 20)] {
            for body in corpus() {
                assert_eq!(
                    contents(&body, threshold, Compression::new(6)).unwrap(),
                    horizon_protocol::frame::contents(&body, threshold).unwrap(),
                    "{} bytes, threshold {threshold:?}",
                    body.len()
                );
            }
        }
    }

    #[test]
    fn every_level_gives_a_stream_that_reads_back_as_the_body() {
        for level in 1..=9 {
            for body in corpus().into_iter().filter(|b| b.len() >= 256) {
                let framed = contents(&body, Some(256), Compression::new(level)).unwrap();
                assert_eq!(
                    inflate(&framed),
                    body,
                    "level {level}, {} bytes",
                    body.len()
                );
            }
        }
    }

    #[test]
    fn the_level_is_the_one_asked_for() {
        // Level 1 and level 9 give the bytes of an encoder of that level, and not those of level 6, on a body
        // whose compression differs between them.
        let body = corpus().swap_remove(7 * 3 + 1); // runs, 16 KiB
        assert_eq!(body.len(), 16 << 10);
        let default = horizon_protocol::frame::contents(&body, Some(256)).unwrap();
        for level in [1, 9] {
            let mut expected = Vec::new();
            horizon_protocol::buf::write_varint(&mut expected, body.len() as i32);
            let mut z = ZlibEncoder::new(expected, Compression::new(level));
            z.write_all(&body).unwrap();
            let expected = z.finish().unwrap();
            let framed = contents(&body, Some(256), Compression::new(level)).unwrap();
            assert_eq!(framed, expected, "level {level}");
            assert_ne!(framed, default, "level {level} gave the default's bytes");
        }
    }

    #[test]
    fn a_packet_of_exactly_the_threshold_is_compressed_at_the_level() {
        // Random bytes of the threshold's length: level 1 and level 6 give different streams for them.
        let body = corpus().swap_remove(3 * 3); // random, 256 bytes
        assert_eq!(body.len(), 256);
        let mut expected = Vec::new();
        horizon_protocol::buf::write_varint(&mut expected, 256);
        let mut z = ZlibEncoder::new(expected, Compression::new(1));
        z.write_all(&body).unwrap();
        let expected = z.finish().unwrap();
        assert_ne!(
            expected,
            horizon_protocol::frame::contents(&body, Some(256)).unwrap()
        );
        assert_eq!(
            contents(&body, Some(256), Compression::new(1)).unwrap(),
            expected
        );
    }

    #[test]
    fn below_the_threshold_or_without_it_nothing_is_compressed_at_any_level() {
        let body = vec![7u8; 255];
        for level in [1, 4, 9] {
            let level = Compression::new(level);
            let mut expected = vec![0];
            expected.extend_from_slice(&body);
            assert_eq!(contents(&body, Some(256), level).unwrap(), expected);
            assert_eq!(contents(&body, None, level).unwrap(), body);
        }
    }

    #[test]
    fn a_too_large_packet_is_refused_at_any_level() {
        let mut seed = 1u32;
        let body: Vec<u8> = (0..(MAX_FRAME + 1024))
            .map(|_| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (seed >> 24) as u8
            })
            .collect();
        for level in [1, 6, 9] {
            let e = contents(&body, Some(256), Compression::new(level)).unwrap_err();
            assert!(
                e.to_string().contains("packet too large"),
                "level {level}: {e}"
            );
        }
    }
}
