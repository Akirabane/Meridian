//! A scripted client for a real run: it connects over TCP to a Horizon server or
//! to the proxy, logs in offline, follows the configuration phase, stays in the
//! world for a number of seconds as a quiet player would, and writes the packets
//! it was sent to a file so that two runs can be compared.
//!
//! ```text
//! real_run run [--host H] [--port P] [--name N] [--seconds S] [--out FILE] [--script SCRIPT]
//! real_run compare FILE_A FILE_B
//! ```
//!
//! `--script "10:/server game1;190:/hub"` makes the player type, at each time in seconds since
//! the connection, a command (the text starts with a slash) or a chat message (it does not).
//! What is due while the server has the player in a configuration phase waits for the world.
//! With a script, the player types nothing else, and `--seconds` defaults to ten seconds past
//! the last time. When the session ends, `run` prints one line per thing typed (`SENT`) and
//! per chat line the server sent (`CHAT`), with the time since the connection, in order; an
//! action bar line is left out. The text of a command may not hold a semicolon.
//!
//! The last line printed by `run` starts with `RESULT` and says how the session
//! ended: `clean` (the client left after its time), `disconnect` (the server sent a
//! disconnect, whose text follows), `closed` (the connection was closed with no
//! packet) or `error`. The exit status is 0 only for `clean`. `moves` counts the times the
//! server took the player back to the configuration phase while they played, as a proxy does
//! to move a player whose server was lost, and the player came out of it. With a script,
//! `said` counts the chat lines the server sent.
use horizon_protocol::ids;
use meridian_testkit::{
    live::{
        End, Entry, Moves, Options, link, parse_script, session_counting, session_linked,
        start_script, transcript,
    },
    trace::{Dir, Recorder},
};
use std::{
    fmt::Write as _,
    process::ExitCode,
    time::{Duration, Instant},
};

const USAGE: &str = "usage: real_run run [--host H] [--port P] [--name N] [--seconds S] [--out FILE] [--script \"10:/server game1;190:/hub\"]\n       real_run compare FILE_A FILE_B";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("run") => run(&args[1..]),
        Some("compare") if args.len() == 3 => compare(&args[1], &args[2]),
        _ => {
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
    }
}

/// The settings of a run.
struct Run {
    options: Options,
    out: Option<String>,
    script: Option<Vec<Entry>>,
}

fn options(args: &[String]) -> Result<Run, String> {
    let mut o = Options {
        host: "127.0.0.1".into(),
        port: 25565,
        name: "Tester".into(),
        seconds: 20,
    };
    let mut out = None;
    let mut typed = None;
    let mut seconds_given = false;
    let mut it = args.iter();
    while let Some(key) = it.next() {
        let value = it.next().ok_or(format!("{key} needs a value"))?;
        match key.as_str() {
            "--host" => o.host = value.clone(),
            "--port" => o.port = value.parse().map_err(|_| "bad --port")?,
            "--name" => o.name = value.clone(),
            "--seconds" => {
                o.seconds = value.parse().map_err(|_| "bad --seconds")?;
                seconds_given = true;
            }
            "--out" => out = Some(value.clone()),
            "--script" => typed = Some(parse_script(value).map_err(|e| format!("--script: {e}"))?),
            other => return Err(format!("unknown option {other}")),
        }
    }
    if let Some(entries) = &typed {
        let last = entries.last().map_or(Duration::ZERO, |e| e.at);
        if !seconds_given {
            o.seconds = last.as_secs_f64().ceil() as u64 + 10;
        } else if last >= Duration::from_secs(o.seconds) {
            return Err(format!(
                "--script has an entry at {:.1} seconds, not before the end of the session (--seconds {})",
                last.as_secs_f64(),
                o.seconds
            ));
        }
    }
    Ok(Run {
        options: o,
        out,
        script: typed,
    })
}

fn run(args: &[String]) -> ExitCode {
    let Run {
        options: o,
        out,
        script,
    } = match options(args) {
        Ok(x) => x,
        Err(e) => {
            eprintln!("real_run: {e}");
            return ExitCode::from(2);
        }
    };
    let recorder = Recorder::new();
    let started = Instant::now();
    let (end, moves, said) = match script {
        None => {
            let moves = Moves::default();
            (session_counting(&o, &recorder, &moves), moves, None)
        }
        Some(entries) => {
            let (handle, link) = link();
            // The times run from the connection; the typist stops with the session.
            let typist = start_script(handle.controller.clone(), entries, started);
            let end = session_linked(&o, &recorder, &link);
            let (lines, said) = transcript(&typist.finish(), &handle.journal);
            for line in lines {
                println!("{line}");
            }
            (end, handle.moves, Some(said))
        }
    };
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
    let said = said.map_or(String::new(), |n| format!(" said={n}"));
    println!(
        "RESULT {word} packets={} bytes={} seconds={:.1} moves={}{said} {detail}",
        trace.count(Dir::ToClient),
        trace.bytes(Dir::ToClient),
        started.elapsed().as_secs_f32(),
        moves.done()
    );
    ExitCode::from(code)
}

/// Compares two files written by `run`. The configuration phase must match
/// packet for packet, and the world's first burst up to the end of the first chunk
/// batch too, leaving out the packets that depend on timing or on the mobs that
/// happened to be around; what follows is only counted.
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
    // Packets that depend on timing or on what happened to be in the world are
    // left out of the comparison: the time of day, pings, keep-alives and the
    // entities that come and go.
    use ids::play::clientbound as play;
    let volatile = [
        play::SET_TIME,
        play::PING,
        play::KEEP_ALIVE,
        play::ADD_ENTITY,
        play::REMOVE_ENTITIES,
        play::SET_ENTITY_DATA,
        play::SET_ENTITY_MOTION,
        play::MOVE_ENTITY_POS,
        play::MOVE_ENTITY_POS_ROT,
        play::MOVE_ENTITY_ROT,
        play::ROTATE_HEAD,
        play::TELEPORT_ENTITY,
        play::SET_EQUIPMENT,
    ];
    let keep = |t: &[(i32, usize)]| -> Vec<i32> {
        t.iter()
            .map(|p| p.0)
            .filter(|id| !volatile.contains(id))
            .collect()
    };
    // The configuration phase is not touched by the filter: its ids are
    // different packets.
    let cfg_end = |t: &[(i32, usize)]| {
        t.iter()
            .position(|(id, _)| *id == ids::configuration::clientbound::FINISH_CONFIGURATION)
            .map_or(0, |n| n + 1)
    };
    let split = |t: &[(i32, usize)], h: usize| -> Vec<i32> {
        let c = cfg_end(t).min(h);
        let mut v: Vec<i32> = t[..c].iter().map(|p| p.0).collect();
        v.extend(keep(&t[c..h]));
        v
    };
    let ids_a = split(&ta, ha);
    let ids_b = split(&tb, hb);
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
