//! `meridian-bench load`: the load baseline. A swarm of bots and synthetic servers in this
//! program, the real `meridian` program in front of them, and a reading of that process once a
//! second. See docs/BASELINE.md for the method, the machine and the figures.
//!
//! The runs are bounded on purpose: no step goes over `--max-bots` bots or `--max-seconds`
//! seconds unless those are raised, and a run stops itself when the machine's free memory falls
//! under `--min-free-gib`.
pub mod bot;
mod micro;
pub mod sample;
pub mod scenario;
mod table;
mod transfer;
pub mod world;

use scenario::{Compression, Settings, Spectator};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use table::{Row, Table};
use world::Shape;

pub const USAGE: &str = "\
Usage: meridian-bench load [OPTIONS]

The load baseline: bots and synthetic servers in this program, the real `meridian` program
in front of them, and the proxy's memory, threads and processor read once a second.

Scenarios (--scenarios, a comma-separated list, default all):
  idle      bots log in, hold the connection, answer keep-alives (also with an empty world)
  active    a stream down and movement up, the latency through the proxy against direct,
            and the three compression settings
  moves     part of the bots are moved to another server while the others play
  churn     connections that come and go as fast as they can
  micro     the cost of a byte of AES and of compression, on its own

Options:
  --scenarios LIST      which of the above
  --players LIST        the bots of each step (default 100,250,500)
  --seconds N           the measured window of a step (default 30, at most --max-seconds)
  --direct-seconds N    the window of the direct phase of `active` (default 20)
  --compression LIST    for `active`: same, server-off, both-off (default all three)
  --view-radius N       chunks around each player at the join: (2N+1)^2 (default 6)
  --entities N          entities each player is sent (default 30)
  --down-rate N         small packets a second, down, of 50 to 200 bytes (default 20)
  --tracked F           the share of them that move an entity the ghost keeps (default 0.6)
  --chunks N            chunk packets a second, down, of 2 to 30 KB (default 2)
  --up-rate N           movement packets a second, up (default 20)
  --probe-rate F        measuring packets a second, up (default 2)
  --keepalive-secs N    seconds between keep-alives (default 15)
  --arrival N           new bots a second; 0 starts them all at once (default 0)
  --movers-percent N    the share of bots that move in `moves` (default 10)
  --churn-cycles N      most connections of a `churn` step (default 3000)
  --churn-hold-ms N     how long a connection of `churn` stays, on average (default 100)
  --max-bots N          the most bots a step may have (default 500)
  --max-seconds N       the longest window (default 60)
  --min-free-gib N      stop when the machine has less memory free (default 16)
  --proxy PATH          the meridian program (default: next to this one)
  --out-dir DIR         write each scenario's rows as CSV there, and the report
  --append              add to the CSV files that are there
  --label TEXT          written in each row: what this pass was (default \"no client\")
  --spectator ADDR      a real server a person's client reaches by --spectator-host, through
                        the same proxy (see docs/BASELINE.md, \"Watching\")
  --spectator-host H    the host name that leads to it (default localhost)
  --forwarding-secret-file PATH
                        the file with the real server's forwarding secret (its `forwarding.secret`):
                        the proxy and the synthetic servers then use it, so the real one accepts the
                        proxy. Without it the run has a secret of its own
  --spectator-port N    the port the proxy listens on then (default 25565)
  -h, --help            show this help";

#[derive(Debug)]
struct Options {
    scenarios: Vec<String>,
    steps: Vec<usize>,
    compressions: Vec<Compression>,
    direct_window: Duration,
    churn_cycles: u64,
    out_dir: Option<PathBuf>,
    append: bool,
    label: String,
    settings: Settings,
}

fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Option<Options>, String> {
    let mut o = Options {
        scenarios: ["idle", "active", "moves", "churn", "micro"]
            .map(str::to_owned)
            .to_vec(),
        steps: vec![100, 250, 500],
        compressions: vec![
            Compression::Same,
            Compression::ServerOff,
            Compression::BothOff,
        ],
        direct_window: Duration::from_secs(20),
        churn_cycles: 3_000,
        out_dir: None,
        append: false,
        label: "no client".into(),
        settings: Settings {
            proxy: PathBuf::new(),
            dir: PathBuf::new(),
            stop: Arc::new(AtomicBool::new(false)),
            window: Duration::from_secs(30),
            shape: Shape {
                view_radius: 6,
                entities: 30,
                registry_bytes: 40_000,
            },
            down_rate: 20,
            tracked: 0.6,
            chunks_per_second: 2.0,
            up_rate: 20,
            probe_rate: 2.0,
            keepalive: Duration::from_secs(15),
            min_free: 16 << 30,
            arrival: 0,
            forwarding: scenario::FORWARDING.to_owned(),
            movers_percent: 10,
            churn_hold: Duration::from_millis(100),
            spectator: None,
        },
    };
    let (mut max_bots, mut max_seconds) = (500usize, 60u64);
    let mut proxy = None;
    let (mut spectator, mut host, mut port) = (None, "localhost".to_owned(), 25565u16);
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let arg = arg
            .into_string()
            .map_err(|a| format!("{a:?} is not text"))?;
        let mut value = |name: &str| {
            args.next()
                .and_then(|v| v.into_string().ok())
                .ok_or_else(|| format!("{name} needs a value"))
        };
        fn number(text: &str, name: &str) -> Result<u64, String> {
            text.parse::<u64>()
                .map_err(|_| format!("{name} takes a number, not {text:?}"))
        }
        fn real(text: &str, name: &str) -> Result<f64, String> {
            text.parse::<f64>()
                .ok()
                .filter(|v| *v >= 0.0)
                .ok_or_else(|| format!("{name} takes a number from 0, not {text:?}"))
        }
        match arg.as_str() {
            "-h" | "--help" => return Ok(None),
            "--scenarios" => {
                o.scenarios = value("--scenarios")?
                    .split(',')
                    .map(|n| n.trim().to_owned())
                    .collect();
                for name in &o.scenarios {
                    if !["idle", "active", "moves", "churn", "micro"].contains(&name.as_str()) {
                        return Err(format!("there is no scenario called {name}"));
                    }
                }
            }
            "--players" => {
                o.steps = value("--players")?
                    .split(',')
                    .map(|n| number(n.trim(), "--players").map(|n| n as usize))
                    .collect::<Result<_, _>>()?;
            }
            "--seconds" => {
                o.settings.window = Duration::from_secs(number(&value("--seconds")?, "--seconds")?)
            }
            "--direct-seconds" => {
                o.direct_window =
                    Duration::from_secs(number(&value("--direct-seconds")?, "--direct-seconds")?);
            }
            "--compression" => {
                o.compressions = value("--compression")?
                    .split(',')
                    .map(|name| match name.trim() {
                        "same" => Ok(Compression::Same),
                        "server-off" => Ok(Compression::ServerOff),
                        "both-off" => Ok(Compression::BothOff),
                        other => Err(format!("there is no compression setting called {other}")),
                    })
                    .collect::<Result<_, _>>()?;
            }
            "--view-radius" => {
                o.settings.shape.view_radius =
                    number(&value("--view-radius")?, "--view-radius")? as i32
            }
            "--entities" => {
                o.settings.shape.entities = number(&value("--entities")?, "--entities")? as usize
            }
            "--down-rate" => {
                o.settings.down_rate = number(&value("--down-rate")?, "--down-rate")? as u32
            }
            "--tracked" => o.settings.tracked = real(&value("--tracked")?, "--tracked")?.min(1.0),
            "--chunks" => o.settings.chunks_per_second = real(&value("--chunks")?, "--chunks")?,
            "--up-rate" => o.settings.up_rate = number(&value("--up-rate")?, "--up-rate")? as u32,
            "--probe-rate" => {
                o.settings.probe_rate = real(&value("--probe-rate")?, "--probe-rate")?
            }
            "--keepalive-secs" => {
                o.settings.keepalive = Duration::from_secs(
                    number(&value("--keepalive-secs")?, "--keepalive-secs")?.max(1),
                );
            }
            "--arrival" => o.settings.arrival = number(&value("--arrival")?, "--arrival")? as u32,
            "--movers-percent" => {
                o.settings.movers_percent =
                    (number(&value("--movers-percent")?, "--movers-percent")? as usize)
                        .clamp(1, 100);
            }
            "--churn-hold-ms" => {
                o.settings.churn_hold =
                    Duration::from_millis(number(&value("--churn-hold-ms")?, "--churn-hold-ms")?);
            }
            "--churn-cycles" => {
                o.churn_cycles = number(&value("--churn-cycles")?, "--churn-cycles")?
            }
            "--max-bots" => max_bots = number(&value("--max-bots")?, "--max-bots")? as usize,
            "--max-seconds" => max_seconds = number(&value("--max-seconds")?, "--max-seconds")?,
            "--min-free-gib" => {
                o.settings.min_free = (real(&value("--min-free-gib")?, "--min-free-gib")?
                    * (1u64 << 30) as f64) as u64
            }
            "--proxy" => proxy = Some(PathBuf::from(value("--proxy")?)),
            "--out-dir" => o.out_dir = Some(PathBuf::from(value("--out-dir")?)),
            "--append" => o.append = true,
            "--label" => o.label = value("--label")?,
            "--spectator" => spectator = Some(value("--spectator")?),
            "--forwarding-secret-file" => {
                let path = value("--forwarding-secret-file")?;
                let text = std::fs::read_to_string(&path)
                    .map_err(|e| format!("cannot read {path}: {e}"))?;
                let secret = text.trim().to_owned();
                if secret.is_empty() || secret.contains(['"', '\\', '\n']) {
                    return Err(format!("{path} holds no usable secret"));
                }
                o.settings.forwarding = secret;
            }
            "--spectator-host" => host = value("--spectator-host")?,
            "--spectator-port" => {
                port = number(&value("--spectator-port")?, "--spectator-port")? as u16
            }
            other => return Err(format!("unknown option {other}")),
        }
    }
    if let Some(&step) = o.steps.iter().find(|&&n| n == 0 || n > max_bots) {
        return Err(format!(
            "a step of {step} bots is not allowed: between 1 and {max_bots} (--max-bots); \
             larger steps are planned with the person whose machine it is before they are run"
        ));
    }
    if o.settings.window > Duration::from_secs(max_seconds)
        || o.direct_window > Duration::from_secs(max_seconds)
    {
        return Err(format!(
            "a window over {max_seconds} s is not allowed (--max-seconds)"
        ));
    }
    if o.settings.window.is_zero() || o.direct_window.is_zero() {
        return Err("a window needs at least a second".into());
    }
    o.settings.spectator = spectator.map(|address| Spectator {
        address,
        host,
        port,
    });
    o.settings.proxy = match proxy {
        Some(path) => path,
        None => std::env::current_exe()
            .map_err(|e| e.to_string())?
            .with_file_name(format!("meridian{}", std::env::consts::EXE_SUFFIX)),
    };
    Ok(Some(o))
}

pub fn main(args: Vec<OsString>) -> ExitCode {
    let mut options = match parse(args) {
        Ok(Some(options)) => options,
        Ok(None) => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Err(message) => {
            eprintln!("meridian-bench load: {message}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let wants_proxy = options.scenarios.iter().any(|s| s != "micro");
    if wants_proxy && !options.settings.proxy.exists() {
        eprintln!(
            "meridian-bench load: {} not found; build it with cargo build --release -p meridian-proxy, \
             or give --proxy",
            options.settings.proxy.display()
        );
        return ExitCode::from(2);
    }
    let stop = Arc::clone(&options.settings.stop);
    {
        let stop = Arc::clone(&stop);
        if let Err(e) = ctrlc::set_handler(move || stop.store(true, Ordering::Relaxed)) {
            eprintln!("meridian-bench load: cannot watch for Ctrl+C: {e}");
            return ExitCode::FAILURE;
        }
    }
    let dir = std::env::temp_dir().join(format!("meridian-load-{}", std::process::id()));
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("meridian-bench load: cannot create {}: {e}", dir.display());
        return ExitCode::FAILURE;
    }
    options.settings.dir = dir.clone();
    let ok = run(&options);
    let _ = std::fs::remove_dir_all(&dir);
    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// What a step gave: its row, or why it did not run.
type Step = Result<Row, String>;

fn run(o: &Options) -> bool {
    let s = &o.settings;
    let mut ok = true;
    let context = crate::context();
    println!(
        "# Load run\n\n- Date: {}\n- Machine: {}\n- Machine load before the run: {}\n- Commit: {} ({} build)\n\
         - Processor time: {}\n- Label: {}\n- Command: {}\n\
         - Window: {} s, direct {} s; view radius {}, {} entities; \
         down {} packets/s, {} chunks/s; up {} packets/s; probes {}/s; keep-alive {} s\n",
        context.date,
        context.machine,
        context.load,
        context.commit,
        context.build,
        context.cpu,
        o.label,
        std::env::args().collect::<Vec<_>>().join(" "),
        s.window.as_secs(),
        o.direct_window.as_secs(),
        s.shape.view_radius,
        s.shape.entities,
        s.down_rate,
        s.chunks_per_second,
        s.up_rate,
        s.probe_rate,
        s.keepalive.as_secs(),
    );
    for name in &o.scenarios {
        let mut table = Table::default();
        if name == "micro" {
            eprintln!("meridian-bench load: micro-measures...");
            table = micro::run(micro::SPAN);
            for row in &mut table.rows {
                row.0.insert(0, ("label", o.label.clone()));
            }
            println!(
                "## micro\n\n{}",
                table.markdown(&["measure", "bytes", "ns_per_byte", "mb_per_s", "wire_ratio"])
            );
        } else {
            for &n in &o.steps {
                if s.stop.load(Ordering::Relaxed) {
                    break;
                }
                // The sockets of the step before leave TIME_WAIT first.
                scenario::let_ports_rest(s, if name == "churn" { 3_000 } else { 6_000 });
                if let Err(why) = scenario::check_memory(s) {
                    eprintln!("meridian-bench load: {why}: stopping");
                    s.stop.store(true, Ordering::Relaxed);
                    ok = false;
                    break;
                }
                let steps: Vec<(String, Step)> = match name.as_str() {
                    "idle" => vec![
                        (format!("idle {n}"), scenario::idle(s, n, false)),
                        (format!("idle bare {n}"), scenario::idle(s, n, true)),
                    ],
                    "active" => o
                        .compressions
                        .iter()
                        .map(|&c| {
                            let mut direct = s.clone();
                            direct.window = o.direct_window;
                            (
                                format!("active {n} {}", c.name()),
                                scenario::active(s, &direct, n, c),
                            )
                        })
                        .collect(),
                    "moves" => vec![(format!("moves {n}"), scenario::moves(s, n))],
                    _ => vec![(format!("churn {n}"), scenario::churn(s, n, o.churn_cycles))],
                };
                for (what, result) in steps {
                    match result {
                        Ok(mut row) => {
                            row.0.insert(0, ("label", o.label.clone()));
                            ok &= row
                                .0
                                .iter()
                                .any(|(k, v)| *k == "proxy_alive" && v == "true");
                            table.push(row);
                        }
                        Err(why) => {
                            eprintln!("meridian-bench load: {what}: {why}");
                            println!("{what}: the step did not run: {why}\n");
                            ok = false;
                        }
                    }
                    if s.stop.load(Ordering::Relaxed) {
                        break;
                    }
                    std::thread::sleep(Duration::from_secs(3));
                }
            }
            print!("## {name}\n\n{}\n", table.markdown(columns(name)));
        }
        if let Some(dir) = &o.out_dir {
            let path = dir.join(format!("{name}.csv"));
            if let Err(e) = write(&table, &path, o.append) {
                eprintln!("meridian-bench load: cannot write {}: {e}", path.display());
                ok = false;
            }
        }
    }
    ok
}

/// The columns each scenario's table shows in the report; the CSV has all of them.
fn columns(scenario: &str) -> &'static [&'static str] {
    match scenario {
        "idle" => &[
            "players",
            "logged_in",
            "failed",
            "login_p50_ms",
            "login_p99_ms",
            "logins_per_s",
            "rss_empty_mib",
            "rss_joined_mib",
            "rss_per_player_kib",
            "threads_joined",
            "cpu_idle_pct",
            "keepalives_sent",
            "keepalives_answered",
            "scenario",
        ],
        "active" => &[
            "players",
            "compression",
            "logged_in",
            "failed",
            "direct_rtt_p50_us",
            "proxy_rtt_p50_us",
            "added_p50_us",
            "added_p99_us",
            "proxy_cpu_mean_pct",
            "proxy_cpu_per_player_pct",
            "rss_per_player_kib",
            "threads_loaded",
            "down_mb_per_s",
        ],
        "moves" => &[
            "players",
            "movers",
            "moved",
            "move_failed",
            "move_p50_ms",
            "move_p95_ms",
            "move_max_ms",
            "delta_kib",
            "rss_before_mib",
            "rss_peak_mib",
            "rss_peak_extra_per_mover_kib",
            "others_rtt_p99_quiet_us",
            "others_rtt_p99_moving_us",
        ],
        _ => &[
            "workers",
            "cycles",
            "failed",
            "cycles_per_s",
            "login_p50_ms",
            "login_p99_ms",
            "proxy_cpu_mean_pct",
            "rss_peak_mib",
            "rss_after_mib",
            "threads_peak",
            "threads_after",
            "time_wait_after",
        ],
    }
}

/// Writes the rows, or adds them to what the file holds when its header is the same.
fn write(table: &Table, path: &Path, append: bool) -> std::io::Result<()> {
    if append && let Ok(old) = std::fs::read_to_string(path) {
        let new = table.csv();
        if let (Some(a), Some(b)) = (old.lines().next(), new.lines().next())
            && a == b
        {
            let body: String = new.lines().skip(1).map(|l| format!("{l}\n")).collect();
            return std::fs::write(path, format!("{old}{body}"));
        }
    }
    table.write(path)
}

#[cfg(test)]
mod tests;
