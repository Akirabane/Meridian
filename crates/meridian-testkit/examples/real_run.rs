//! A scripted client for a real run: it connects over TCP to a Horizon server or
//! to the proxy, logs in offline, follows the configuration phase, stays in the
//! world for a number of seconds as a quiet player would, and writes the packets
//! it was sent to a file so that two runs can be compared.
//!
//! ```text
//! real_run run [--host H] [--port P] [--name N] [--seconds S] [--out FILE]
//! real_run compare FILE_A FILE_B
//! ```
//!
//! The last line printed by `run` starts with `RESULT` and says how the session
//! ended: `clean` (the client left after its time), `disconnect` (the server sent a
//! disconnect, whose text follows), `closed` (the connection was closed with no
//! packet) or `error`. The exit status is 0 only for `clean`.
use horizon_protocol::ids;
use meridian_testkit::{
    live::{End, Options, session},
    trace::{Dir, Recorder},
};
use std::{fmt::Write as _, process::ExitCode, time::Instant};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("run") => run(&args[1..]),
        Some("compare") if args.len() == 3 => compare(&args[1], &args[2]),
        _ => {
            eprintln!(
                "usage: real_run run [--host H] [--port P] [--name N] [--seconds S] [--out FILE]\n       real_run compare FILE_A FILE_B"
            );
            ExitCode::from(2)
        }
    }
}

fn options(args: &[String]) -> Result<(Options, Option<String>), String> {
    let mut o = Options {
        host: "127.0.0.1".into(),
        port: 25565,
        name: "Tester".into(),
        seconds: 20,
    };
    let mut out = None;
    let mut it = args.iter();
    while let Some(key) = it.next() {
        let value = it.next().ok_or(format!("{key} needs a value"))?;
        match key.as_str() {
            "--host" => o.host = value.clone(),
            "--port" => o.port = value.parse().map_err(|_| "bad --port")?,
            "--name" => o.name = value.clone(),
            "--seconds" => o.seconds = value.parse().map_err(|_| "bad --seconds")?,
            "--out" => out = Some(value.clone()),
            other => return Err(format!("unknown option {other}")),
        }
    }
    Ok((o, out))
}

fn run(args: &[String]) -> ExitCode {
    let (o, out) = match options(args) {
        Ok(x) => x,
        Err(e) => {
            eprintln!("real_run: {e}");
            return ExitCode::from(2);
        }
    };
    let recorder = Recorder::new();
    let started = Instant::now();
    let end = session(&o, &recorder);
    let trace = recorder.trace();
    if let Some(path) = &out {
        let mut text = String::new();
        for p in trace.packets(Dir::ToClient) {
            let _ = writeln!(text, "{} {}", p.id, p.payload.len());
        }
        if let Err(e) = std::fs::write(path, text) {
            eprintln!("real_run: cannot write {path}: {e}");
        }
    }
    let (word, detail, code) = match &end {
        End::Clean => ("clean", String::new(), 0),
        End::Disconnect(text) => ("disconnect", text.clone(), 1),
        End::Closed => ("closed", String::new(), 1),
        End::Error(e) => ("error", e.clone(), 1),
    };
    println!(
        "RESULT {word} packets={} bytes={} seconds={:.1} {detail}",
        trace.count(Dir::ToClient),
        trace.bytes(Dir::ToClient),
        started.elapsed().as_secs_f32()
    );
    ExitCode::from(code)
}

/// Compares two files written by `run`. The configuration phase must match
/// packet for packet, and the world's first burst up to the end of the first chunk
/// batch too; what follows (keep-alives, time, mobs) is only counted.
fn compare(a: &str, b: &str) -> ExitCode {
    let read = |path: &str| -> Result<Vec<(i32, usize)>, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
        Ok(text
            .lines()
            .filter_map(|l| {
                let mut it = l.split_whitespace();
                Some((it.next()?.parse().ok()?, it.next()?.parse().ok()?))
            })
            .collect())
    };
    let (ta, tb) = match (read(a), read(b)) {
        (Ok(x), Ok(y)) => (x, y),
        (Err(e), _) | (_, Err(e)) => {
            eprintln!("real_run: {e}");
            return ExitCode::from(2);
        }
    };
    // The cut: the first chunk batch finished packet after the configuration
    // phase (the phases share ids, so find the finish-configuration packet first).
    let head = |t: &[(i32, usize)]| -> usize {
        let finish = t
            .iter()
            .position(|(id, _)| *id == ids::configuration::clientbound::FINISH_CONFIGURATION)
            .unwrap_or(0);
        t.iter()
            .skip(finish + 1)
            .position(|(id, _)| *id == ids::play::clientbound::CHUNK_BATCH_FINISHED)
            .map_or(t.len(), |n| finish + 1 + n + 1)
    };
    let (ha, hb) = (head(&ta), head(&tb));
    let ids_a: Vec<i32> = ta[..ha].iter().map(|p| p.0).collect();
    let ids_b: Vec<i32> = tb[..hb].iter().map(|p| p.0).collect();
    println!(
        "compared the first {} and {} packets (up to the first chunk batch); {} and {} in all",
        ha,
        hb,
        ta.len(),
        tb.len()
    );
    if ids_a == ids_b {
        println!("SAME ids in the same order");
        ExitCode::SUCCESS
    } else {
        let at = ids_a
            .iter()
            .zip(&ids_b)
            .position(|(x, y)| x != y)
            .unwrap_or(ids_a.len().min(ids_b.len()));
        println!(
            "DIFFERENT at packet {at}: {:?} against {:?}",
            ids_a.get(at),
            ids_b.get(at)
        );
        ExitCode::from(1)
    }
}
