//! The relay to the client writes a read's packets together: the bytes are the same as one
//! write each, and nothing waits for more packets.
use super::*;
use crate::session::frame;
use horizon_protocol::connection::Pipe;
use meridian_testkit::body;
use std::io::Read;

/// What reaches the other end of a pipe when `bodies` are sent through a writer with
/// `compression` and `key`: one `send` each, or framed together and written once.
fn wire(
    compression: Option<i32>,
    key: Option<[u8; 16]>,
    together: bool,
    bodies: &[Vec<u8>],
) -> Vec<u8> {
    let (near, mut far) = Pipe::pair();
    let mut connection = Connection::local(near);
    if let Some(threshold) = compression {
        connection.set_compression(threshold);
    }
    if let Some(key) = key {
        connection.enable_encryption(&key);
    }
    let (_reader, mut writer) = connection.split();
    if together {
        let mut out = Vec::new();
        for body in bodies {
            frame(&mut writer, body, &mut out).unwrap();
        }
        writer.write_raw(&out).unwrap();
    } else {
        for body in bodies {
            writer.send(body).unwrap();
        }
    }
    far.set_nonblocking(true);
    let mut wire = Vec::new();
    let mut buffer = [0u8; 1 << 16];
    while let Ok(n) = far.read(&mut buffer) {
        if n == 0 {
            break;
        }
        wire.extend_from_slice(&buffer[..n]);
    }
    wire
}

/// Packet bodies around the compression threshold, large ones that compress, one that does not,
/// and one with an empty payload.
fn corpus() -> Vec<Vec<u8>> {
    let mut bodies = vec![body(pc::KEEP_ALIVE, &7u64.to_be_bytes()), vec![0x0a]];
    for size in [10, 254, 255, 256, 257, 1_000, 20_000] {
        let payload: Vec<u8> = (0..size).map(|i| (i % 7) as u8).collect();
        bodies.push(body(pc::CUSTOM_PAYLOAD, &payload));
    }
    let mut noise = 0x9e37_79b9u32;
    let random: Vec<u8> = (0..3_000)
        .map(|_| {
            noise ^= noise << 13;
            noise ^= noise >> 17;
            noise ^= noise << 5;
            noise as u8
        })
        .collect();
    bodies.push(body(pc::CUSTOM_PAYLOAD, &random));
    bodies
}

/// T1: framed together and written once, the bytes are those of one `send` each, with and
/// without compression, with and without encryption.
#[test]
fn packets_written_together_are_the_same_bytes() {
    let bodies = corpus();
    for compression in [None, Some(256)] {
        for key in [None, Some([42u8; 16])] {
            let one_by_one = wire(compression, key, false, &bodies);
            let together = wire(compression, key, true, &bodies);
            assert!(!one_by_one.is_empty());
            assert_eq!(
                together,
                one_by_one,
                "compression {compression:?}, encryption {}",
                key.is_some()
            );
        }
    }
}

/// What a batch's buffers keep between batches stays small after a large one (a join): a
/// buffer kept at its largest for the whole connection cost each player tens of KiB.
#[test]
fn a_large_batch_does_not_stay_in_memory() {
    let mut out: Vec<u8> = Vec::with_capacity(128 << 10);
    crate::session::keep_small(&mut out, 16 << 10);
    assert!(out.capacity() <= 16 << 10, "{}", out.capacity());
    // A buffer that a tick's batch fits in is left as it is: no allocation each tick.
    let mut small: Vec<u8> = Vec::with_capacity(8 << 10);
    crate::session::keep_small(&mut small, 16 << 10);
    assert!(small.capacity() >= 8 << 10);
    let mut batch: Vec<(i32, Vec<u8>)> = Vec::with_capacity(5_000);
    crate::session::keep_small(&mut batch, 256);
    assert!(batch.capacity() <= 256, "{}", batch.capacity());
}

/// The bound keeps the grouping of a game tick: a typical player's tick, 30 packets (small ones
/// and a chunk's worth), written by the server at once, still reaches the client in one write.
#[test]
fn a_tick_of_packets_is_still_one_write() {
    let mut bench = bench();
    let before = writes(&bench);
    let mut out = Vec::new();
    for i in 0..29u64 {
        frame(
            &mut bench.source.writer,
            &body(pc::KEEP_ALIVE, &i.to_be_bytes()),
            &mut out,
        )
        .unwrap();
    }
    let chunk: Vec<u8> = (0..8_000u32).map(|i| (i % 13) as u8).collect();
    frame(
        &mut bench.source.writer,
        &body(pc::CUSTOM_PAYLOAD, &chunk),
        &mut out,
    )
    .unwrap();
    bench.source.writer.write_raw(&out).unwrap();
    for i in 0..29u64 {
        assert_eq!(
            expect(&mut bench.player, pc::KEEP_ALIVE).unwrap(),
            i.to_be_bytes()
        );
    }
    assert_eq!(
        expect(&mut bench.player, pc::CUSTOM_PAYLOAD).unwrap(),
        chunk
    );
    assert_eq!(
        writes(&bench) - before,
        1,
        "30 packets of a tick, one write"
    );
}

/// A read that brings many packets (a join's world) is written in bounded batches: at most
/// `BATCH_PACKETS` packets, or about `BATCH_BYTES` of payload, each, so that a session never
/// holds a whole read's inflated packets at once. Every packet still arrives, in order.
#[test]
fn a_large_read_is_written_in_bounded_batches() {
    use crate::session::{BATCH_BYTES, BATCH_PACKETS};
    let mut bench = bench();
    // 5,000 small packets, written by the backend at once.
    let before = writes(&bench);
    let mut out = Vec::new();
    for i in 0..5_000u64 {
        frame(
            &mut bench.source.writer,
            &body(pc::KEEP_ALIVE, &i.to_be_bytes()),
            &mut out,
        )
        .unwrap();
    }
    bench.source.writer.write_raw(&out).unwrap();
    for i in 0..5_000u64 {
        assert_eq!(
            expect(&mut bench.player, pc::KEEP_ALIVE).unwrap(),
            i.to_be_bytes()
        );
    }
    let made = writes(&bench) - before;
    assert!(
        made >= 5_000u64.div_ceil(BATCH_PACKETS as u64),
        "{made} writes for 5,000 packets: a batch took more than {BATCH_PACKETS}"
    );
    // Ten large packets at once: no batch holds much more than the byte bound.
    let large = BATCH_BYTES * 3 / 5;
    let before = writes(&bench);
    out.clear();
    for i in 0..10u8 {
        let payload = vec![i; large];
        frame(
            &mut bench.source.writer,
            &body(pc::CUSTOM_PAYLOAD, &payload),
            &mut out,
        )
        .unwrap();
    }
    bench.source.writer.write_raw(&out).unwrap();
    for i in 0..10u8 {
        let payload = expect(&mut bench.player, pc::CUSTOM_PAYLOAD).unwrap();
        assert_eq!(payload.len(), large);
        assert_eq!(payload[0], i);
    }
    let made = writes(&bench) - before;
    assert!(
        made >= 5,
        "{made} writes for 10 packets of {large} bytes: a batch held more than two"
    );
}

/// The writes the session made to its client so far.
fn writes(bench: &Bench) -> u64 {
    bench.handle.0.down().writes
}

/// T2: a backend writes twenty packets at once, as a server writes a tick's: the client gets
/// them all, in order, in one write of the relay.
#[test]
fn a_burst_of_packets_reaches_the_client_in_order_in_one_write() {
    let mut bench = bench();
    let before = writes(&bench);
    let mut out = Vec::new();
    for i in 0..20u64 {
        frame(
            &mut bench.source.writer,
            &body(pc::KEEP_ALIVE, &i.to_be_bytes()),
            &mut out,
        )
        .unwrap();
    }
    bench.source.writer.write_raw(&out).unwrap();
    for i in 0..20u64 {
        assert_eq!(
            expect(&mut bench.player, pc::KEEP_ALIVE).unwrap(),
            i.to_be_bytes()
        );
    }
    assert_eq!(writes(&bench), before + 1, "one write for the twenty");
}

/// T3: a packet that comes alone is relayed at once: nothing waits for others to join it.
#[test]
fn a_packet_alone_is_relayed_at_once() {
    let mut bench = bench();
    for i in 0..3u64 {
        let before = writes(&bench);
        send(&mut bench.source, pc::KEEP_ALIVE, &i.to_be_bytes());
        assert_eq!(
            expect(&mut bench.player, pc::KEEP_ALIVE).unwrap(),
            i.to_be_bytes()
        );
        assert_eq!(writes(&bench), before + 1);
    }
}

/// The bytes on the wire of `bodies` framed at `level` with `threshold`: what the client reads for them.
pub(super) fn framed_len(bodies: &[Vec<u8>], threshold: usize, level: u32) -> usize {
    bodies
        .iter()
        .map(|body| {
            let contents =
                crate::compress::contents(body, Some(threshold), flate2::Compression::new(level))
                    .unwrap();
            let mut prefix = Vec::new();
            horizon_protocol::buf::write_varint(&mut prefix, contents.len() as i32);
            prefix.len() + contents.len()
        })
        .sum()
}

/// A session set up as [`bench`], with compression at `threshold` between the proxy and the client and, when
/// `level` is given, [`Session::with_compression_level`].
pub(super) fn compressed_bench(threshold: i32, level: Option<u32>) -> Bench {
    let (mut proxy_client, mut player) = pipe();
    proxy_client.set_compression(threshold);
    player.set_compression(threshold);
    let (proxy_backend, mut source) = pipe();
    let client = ClientSession {
        profile: profile(),
        auth: AuthKind::Offline,
        virtual_host: None,
        connection: proxy_client,
    };
    let backend = BackendLink {
        connection: proxy_backend,
    };
    let (session, handle) = Session::new(client, backend, "source").unwrap();
    let session = match level {
        Some(level) => session.with_compression_level(level),
        None => session,
    };
    let run = std::thread::spawn(move || session.run());
    join_source_as(&mut player, &mut source, &INFORMATION);
    Bench {
        handle,
        run,
        player,
        source,
    }
}

/// The relay compresses at the session's level: a large packet from the backend reaches the client in exactly the
/// bytes of a frame of that level, and without the call at the default level, 6.
#[test]
fn the_relay_compresses_at_the_session_level() {
    let payload: Vec<u8> = (0..20_000u32).map(|i| ((i / 7) % 13) as u8).collect();
    let packet = body(pc::CUSTOM_PAYLOAD, &payload);
    let sizes: Vec<usize> = [1, 6, 9]
        .iter()
        .map(|&l| framed_len(std::slice::from_ref(&packet), 256, l))
        .collect();
    assert!(
        sizes[0] != sizes[1] && sizes[1] != sizes[2],
        "{sizes:?}: the levels must differ on this body"
    );
    for (level, expected) in [(None, sizes[1]), (Some(1), sizes[0]), (Some(9), sizes[2])] {
        let mut bench = compressed_bench(256, level);
        let counted = &bench.player.reader.counters.bytes_in;
        let before = counted.load(std::sync::atomic::Ordering::Relaxed);
        send(&mut bench.source, pc::CUSTOM_PAYLOAD, &payload);
        assert_eq!(
            expect(&mut bench.player, pc::CUSTOM_PAYLOAD).unwrap(),
            payload
        );
        let got = bench
            .player
            .reader
            .counters
            .bytes_in
            .load(std::sync::atomic::Ordering::Relaxed)
            - before;
        assert_eq!(got as usize, expected, "level {level:?}");
    }
}
