//! `meridian-bench`: measures a Meridian proxy on one machine, over 127.0.0.1,
//! against a direct connection to the same stand-in backend. See docs/BENCH.md.
mod backend;
mod cpu;
mod echo;
mod player;
mod proxy;
mod report;
mod run;
mod stats;

use backend::Backend;
use proxy::Proxy;
use run::Load;
use std::ffi::OsString;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const USAGE: &str = "\
Usage: meridian-bench [OPTIONS]

Measures the proxy on this machine, over 127.0.0.1: the same players and packets
directly to a stand-in backend, then through the real `meridian` program.

Options:
  --players LIST   player counts, one step each (default 10,100)
  --seconds N      the measured window of each phase (default 20)
  --rate N         measuring packets a second, per player (default 20)
  --size N         bytes of data in each measuring packet (default 64)
  --proxy PATH     the meridian program (default: next to this one)
  --out PATH       also write the report to this file
  -h, --help       show this help";

/// The secret the proxy and the stand-in backend share for a run.
const SECRET: &str = "meridian-bench-local-secret";

struct Options {
    steps: Vec<usize>,
    load: Load,
    proxy: PathBuf,
    out: Option<PathBuf>,
}

fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Option<Options>, String> {
    let mut steps = vec![10, 100];
    let (mut seconds, mut rate, mut size) = (20u64, 20u32, 64usize);
    let mut proxy = None;
    let mut out = None;
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
        let number = |text: String, name: &str| {
            text.parse::<u64>()
                .ok()
                .filter(|n| *n > 0)
                .ok_or_else(|| format!("{name} takes a number above 0, not {text:?}"))
        };
        match arg.as_str() {
            "-h" | "--help" => return Ok(None),
            "--players" => {
                steps = value("--players")?
                    .split(',')
                    .map(|n| number(n.trim().to_owned(), "--players").map(|n| n as usize))
                    .collect::<Result<_, _>>()?;
            }
            "--seconds" => seconds = number(value("--seconds")?, "--seconds")?,
            "--rate" => rate = number(value("--rate")?, "--rate")? as u32,
            "--size" => size = number(value("--size")?, "--size")? as usize,
            "--proxy" => proxy = Some(PathBuf::from(value("--proxy")?)),
            "--out" => out = Some(PathBuf::from(value("--out")?)),
            other => return Err(format!("unknown option {other}")),
        }
    }
    let proxy = match proxy {
        Some(path) => path,
        None => std::env::current_exe()
            .map_err(|e| e.to_string())?
            .with_file_name(format!("meridian{}", std::env::consts::EXE_SUFFIX)),
    };
    Ok(Some(Options {
        steps,
        load: Load {
            players: 0,
            window: Duration::from_secs(seconds),
            rate,
            size,
        },
        proxy,
        out,
    }))
}

fn main() -> ExitCode {
    let options = match parse(std::env::args_os().skip(1)) {
        Ok(Some(options)) => options,
        Ok(None) => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Err(message) => {
            eprintln!("meridian-bench: {message}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    if !options.proxy.exists() {
        eprintln!(
            "meridian-bench: {} not found; build it with cargo build --release -p meridian-proxy, \
             or give --proxy",
            options.proxy.display()
        );
        return ExitCode::from(2);
    }
    // Ctrl+C stops the players; the proxy, in the same console, stops on it too,
    // and is killed below if it has not.
    let stop = Arc::new(AtomicBool::new(false));
    {
        let stop = Arc::clone(&stop);
        if let Err(e) = ctrlc::set_handler(move || stop.store(true, Ordering::Relaxed)) {
            eprintln!("meridian-bench: cannot watch for Ctrl+C: {e}");
            return ExitCode::FAILURE;
        }
    }
    let dir = std::env::temp_dir().join(format!("meridian-bench-{}", std::process::id()));
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("meridian-bench: cannot create {}: {e}", dir.display());
        return ExitCode::FAILURE;
    }
    let mut text = report::header(&context(), &options.load, &options.steps);
    print!("{text}");
    let mut ok = true;
    for &players in &options.steps {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        let load = Load {
            players,
            ..options.load
        };
        eprintln!("meridian-bench: {players} players...");
        match step(load, &options.proxy, &dir, &stop) {
            Ok(step) => {
                let part = report::step(&step);
                print!("{part}");
                text.push_str(&part);
                ok &= step.proxy_stopped;
            }
            Err(e) => {
                let part = format!("\n## {players} players\n\nThe step did not run: {e}\n");
                print!("{part}");
                text.push_str(&part);
                ok = false;
            }
        }
        // Let the closed connections leave TIME_WAIT before the next step.
        std::thread::sleep(Duration::from_secs(2));
    }
    let _ = std::fs::remove_dir_all(&dir);
    if let Some(out) = &options.out
        && let Err(e) = std::fs::write(out, &text)
    {
        eprintln!("meridian-bench: cannot write {}: {e}", out.display());
        ok = false;
    }
    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// One step: the load directly, then through a proxy started for it.
fn step(
    load: Load,
    binary: &std::path::Path,
    dir: &std::path::Path,
    stop: &Arc<AtomicBool>,
) -> std::io::Result<report::Step> {
    let (direct, direct_backend_logins) = {
        let backend = Backend::start(None)?;
        let phase = run::phase(backend.address, load, None, stop);
        (phase, backend.logged_in.load(Ordering::Relaxed))
    };
    let backend = Backend::start(Some(SECRET.as_bytes().to_vec()))?;
    let proxy = Proxy::start(binary, backend.address, SECRET, dir)?;
    // A moment for the proxy to settle before its idle size is read.
    std::thread::sleep(Duration::from_millis(500));
    let pid = proxy.pid;
    let proxy_idle_memory = run::memory_of(pid);
    let proxied = run::phase(proxy.address, load, Some(pid), stop);
    let proxy_warnings = proxy.warnings();
    let backend_refused = backend.refused.load(Ordering::Relaxed);
    let proxied_backend_logins = backend.logged_in.load(Ordering::Relaxed);
    proxy.stop()?;
    let mut proxied = proxied;
    if backend_refused > 0 && proxied.failures.len() < 3 {
        proxied
            .failures
            .push(format!("the backend refused {backend_refused} logins"));
    }
    Ok(report::Step {
        load,
        direct,
        proxied,
        proxy_idle_memory,
        proxy_warnings,
        proxy_stopped: !proxy::still_running(pid),
        backend_logins: [direct_backend_logins, proxied_backend_logins],
    })
}

/// The command, the date, the machine and the commit, for the report.
fn context() -> report::Context {
    let mut system = sysinfo::System::new();
    system.refresh_cpu_all();
    system.refresh_memory();
    let cpu = system
        .cpus()
        .first()
        .map(|c| c.brand().trim().to_owned())
        .unwrap_or_default();
    let machine = format!(
        "{} {}, {cpu}, {} logical cores, {:.1} GiB of memory",
        sysinfo::System::name().unwrap_or_default(),
        sysinfo::System::os_version().unwrap_or_default(),
        system.cpus().len(),
        system.total_memory() as f64 / 1024.0 / 1024.0 / 1024.0
    );
    // What else the machine was doing, over one second before the run.
    std::thread::sleep(Duration::from_secs(1));
    system.refresh_cpu_all();
    system.refresh_memory();
    let load = format!(
        "{:.0} % of one core in use, out of {}, and {:.1} GiB of memory available",
        system.global_cpu_usage() * system.cpus().len() as f32,
        system.cpus().len() * 100,
        system.available_memory() as f64 / 1024.0 / 1024.0 / 1024.0
    );
    let commit = std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .unwrap_or_else(|| "unknown".to_owned());
    report::Context {
        cpu: cpu::method(),
        command: std::env::args().collect::<Vec<_>>().join(" "),
        date: utc_now(),
        machine,
        load,
        commit,
        build: if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        },
    }
}

/// The time as `YYYY-MM-DDTHH:MM:SSZ`, in UTC.
fn utc_now() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    // Howard Hinnant's civil_from_days.
    let z = (secs / 86_400) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    let rest = secs % 86_400;
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rest / 3600,
        rest / 60 % 60,
        rest % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<OsString> {
        list.iter().map(OsString::from).collect()
    }

    #[test]
    fn options_read_and_check_their_values() {
        let o = parse(args(&[
            "--players",
            "10, 500,1000",
            "--seconds",
            "5",
            "--proxy",
            "x",
        ]))
        .unwrap()
        .unwrap();
        assert_eq!(o.steps, [10, 500, 1000]);
        assert_eq!(o.load.window, Duration::from_secs(5));
        assert_eq!(o.load.rate, 20);
        assert_eq!(o.proxy, PathBuf::from("x"));
        assert!(parse(args(&["--players", "0"])).is_err());
        assert!(parse(args(&["--rate"])).is_err());
        assert!(parse(args(&["--port", "1"])).is_err());
        assert!(parse(args(&["--help"])).unwrap().is_none());
    }

    #[test]
    fn the_date_is_written_in_utc() {
        let date = utc_now();
        assert_eq!(date.len(), 20, "{date}");
        assert!(date.ends_with('Z') && date.as_bytes()[10] == b'T', "{date}");
    }

    #[test]
    fn the_proxy_configuration_has_no_limits_and_one_backend() {
        let text = proxy::configuration(
            "127.0.0.1:40001".parse().unwrap(),
            "127.0.0.1:40002".parse().unwrap(),
            SECRET,
        );
        for needle in [
            "listen = \"127.0.0.1:40001\"",
            "max-players = 0",
            "connections-per-minute = 0",
            "auth-mode = \"offline\"",
            "address = \"127.0.0.1:40002\"",
        ] {
            assert!(text.contains(needle), "{needle} missing from {text}");
        }
    }
}
