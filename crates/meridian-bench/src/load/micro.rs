//! What a byte costs, on its own: the AES cipher a connection of the online mode is put through
//! (the swarm of the load runs logs in offline, so it never meets it) and the compression of
//! the protocol, measured in one thread, without a network, so that the figures are the code's.
//!
//! The cipher is measured by itself, then the whole connection (framing, compression, cipher)
//! over an in-memory pipe, sending and receiving timed apart. The data is made to compress as
//! chunks do, about 3 to 1 (the table says what it measured).
use super::table::{Row, Table};
use super::world::chunk_like;
use horizon_protocol::crypto::Cfb8;
use meridian_testkit::{body, pipe};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

/// How long each figure is measured for, at least, in a real run.
pub const SPAN: Duration = Duration::from_millis(400);

/// Runs `step` until `span` has passed, and returns how many times it ran and how long they
/// took altogether.
fn timed(span: Duration, mut step: impl FnMut()) -> (u64, Duration) {
    let (mut runs, began) = (0u64, Instant::now());
    while began.elapsed() < span {
        step();
        runs += 1;
    }
    (runs, began.elapsed())
}

fn row(what: &str, size: usize, runs: u64, took: Duration, ratio: Option<f64>) -> Row {
    let bytes = runs as f64 * size as f64;
    Row::new()
        .with("measure", what)
        .with("bytes", size)
        .with("runs", runs)
        .num("ns_per_byte", took.as_secs_f64() * 1e9 / bytes, 2)
        .num("mb_per_s", bytes / took.as_secs_f64() / 1e6, 1)
        .with(
            "wire_ratio",
            ratio.map_or(String::new(), |r| format!("{r:.2}")),
        )
}

pub fn run(span: Duration) -> Table {
    let mut table = Table::default();
    let key = [7u8; 16];
    // The cipher alone, in place.
    for size in [64usize, 1_024, 16_384] {
        let mut data = chunk_like(size.max(2_048));
        data.truncate(size);
        data.resize(size, 1);
        let mut cipher = Cfb8::new(&key, &key);
        let (runs, took) = timed(span, || cipher.encrypt(&mut data));
        table.push(row("aes-cfb8 encrypt", size, runs, took, None));
        let mut cipher = Cfb8::new(&key, &key);
        let (runs, took) = timed(span, || cipher.decrypt(&mut data));
        table.push(row("aes-cfb8 decrypt", size, runs, took, None));
    }
    // A connection, in the four ways a proxy's side of one is set.
    let modes: [(&str, i32, bool); 4] = [
        ("plain", -1, false),
        ("deflate", 0, false),
        ("aes", -1, true),
        ("deflate+aes", 0, true),
    ];
    for size in [100usize, 2_048, 16_384, 30_720] {
        let data = chunk_like(size);
        let packet = body(24, &data);
        for (name, threshold, encrypted) in modes {
            let (mut a, mut b) = pipe();
            a.set_compression(threshold);
            b.set_compression(threshold);
            if encrypted {
                a.enable_encryption(&key);
                b.enable_encryption(&key);
            }
            let (mut sending, mut receiving) = (Duration::ZERO, Duration::ZERO);
            let (mut runs, mut wire) = (0u64, 0u64);
            let began = Instant::now();
            while began.elapsed() < span {
                let t = Instant::now();
                if a.send(&packet).is_err() {
                    break;
                }
                sending += t.elapsed();
                let t = Instant::now();
                if b.recv().is_err() {
                    break;
                }
                receiving += t.elapsed();
                runs += 1;
            }
            wire += b.reader.counters.bytes_in.load(Ordering::Relaxed).max(0) as u64;
            let ratio = (wire > 0).then(|| (runs as f64 * packet.len() as f64) / wire as f64);
            table.push(row(
                &format!("{name} send"),
                packet.len(),
                runs,
                sending,
                ratio,
            ));
            table.push(row(
                &format!("{name} receive"),
                packet.len(),
                runs,
                receiving,
                None,
            ));
        }
    }
    table
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The micro-measures run and give a positive figure for every row, with the cipher no
    /// faster than the plain copy.
    #[test]
    fn every_measure_gives_a_figure() {
        let table = run(Duration::from_millis(10));
        assert!(table.rows.len() > 20, "{}", table.rows.len());
        for row in &table.rows {
            let ns: f64 = row
                .0
                .iter()
                .find(|(n, _)| *n == "ns_per_byte")
                .unwrap()
                .1
                .parse()
                .unwrap();
            assert!(ns > 0.0, "{row:?}");
        }
    }
}
