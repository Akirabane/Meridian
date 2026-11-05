//! A proxy on another machine. The bots and the synthetic servers stay here; the proxy runs
//! elsewhere (a service on a Linux machine of the network, say), reached at an address, and
//! reaching the servers at this machine's network address and a range of ports.
//!
//! What cannot be read from here is the proxy process: its memory, threads and processor time. The
//! run writes what it can see from the bots' side, and **when each phase began and ended**
//! (`windows.csv`, in UTC and in milliseconds since the epoch), so that whoever reads the proxy
//! on its machine takes the same windows.
//!
//! The proxy's configuration is made by this program ([`write_config`]), with a secret of its own
//! each time, written in that file and nowhere else; a run reads the secret back from the file.
use super::bot::{Live, Role};
use super::sample::{Sampler, Watch, mean, mib, peak};
use super::scenario::{
    Compression, Server, Settings, Spec, WaveResult, active_traffic, arrive, configuration, end,
    first_failure, ms, pause, play, start_world, wave_results,
};
use super::table::{Row, Table};
use super::world::Traffic;
use crate::stats::Summary;
use meridian_config::Config;
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Connections made to measure the network between the bots and the proxy.
const RAW_SAMPLES: usize = 50;

/// The proxy's place, and the servers'.
#[derive(Clone, Debug)]
pub struct Remote {
    /// Where the bots connect.
    pub proxy: SocketAddr,
    /// The address of this machine by which the proxy reaches the synthetic servers.
    pub bind: Ipv4Addr,
    /// The first and the last port the servers may use; the first is the lobby's.
    pub ports: (u16, u16),
    /// The compression threshold the proxy's configuration gives the players' side.
    pub threshold: i32,
}

impl Remote {
    /// Where the server of this slot (0 is the lobby) listens.
    pub fn server_address(&self, slot: usize) -> SocketAddr {
        SocketAddr::from((self.bind, self.ports.0 + slot as u16))
    }
}

/// `A-B` as a first and a last port, with room for two servers.
pub fn parse_ports(text: &str) -> Result<(u16, u16), String> {
    let bad = || format!("--backend-ports takes a range such as 25701-25710, not {text:?}");
    let (a, b) = text.split_once('-').ok_or_else(bad)?;
    let (a, b): (u16, u16) = (
        a.trim().parse().map_err(|_| bad())?,
        b.trim().parse().map_err(|_| bad())?,
    );
    if a < 1024 || b <= a {
        return Err(format!(
            "--backend-ports: {a}-{b} is not a range of unprivileged ports with at least two"
        ));
    }
    Ok((a, b))
}

// ---------------------------------------------------------------------------------------------
// The phases' windows

#[derive(Clone, Debug)]
struct Window {
    step: String,
    players: usize,
    phase: String,
    from: u64,
    to: u64,
}

/// When each phase of each step began and ended.
#[derive(Debug, Default)]
pub struct Windows(Mutex<Vec<Window>>);

impl Windows {
    /// Now, in milliseconds since the epoch (UTC).
    pub fn now_ms() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64
    }

    pub fn record(&self, step: &str, players: usize, phase: &str, from: u64, to: u64) {
        if let Ok(mut all) = self.0.lock() {
            all.push(Window {
                step: step.to_owned(),
                players,
                phase: phase.to_owned(),
                from,
                to,
            });
        }
    }

    /// One row a phase.
    pub fn table(&self) -> Table {
        let mut table = Table::default();
        for w in self.0.lock().map(|g| g.clone()).unwrap_or_default() {
            table.push(
                Row::new()
                    .with("step", &w.step)
                    .with("players", w.players)
                    .with("phase", &w.phase)
                    .with("start_utc", crate::utc_at(w.from / 1000))
                    .with("end_utc", crate::utc_at(w.to / 1000))
                    .with("start_epoch_ms", w.from)
                    .with("end_epoch_ms", w.to)
                    .num("seconds", w.to.saturating_sub(w.from) as f64 / 1000.0, 1),
            );
        }
        table
    }
}

// ---------------------------------------------------------------------------------------------
// The proxy's configuration

/// 32 hexadecimal characters from the system's random bytes.
pub fn fresh_secret() -> Result<String, String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|e| format!("no random bytes from the system: {e}"))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// The text of the remote proxy's configuration: the same one a local run writes, listening where
/// the bots connect, with the lobby's synthetic server behind it.
pub fn generate(remote: &Remote, threshold: i32, secret: &str) -> String {
    let lobby = Server {
        name: "lobby",
        address: remote.server_address(0),
        sidecar: None,
    };
    format!(
        "# Written by `meridian-bench load --write-remote-config`: the proxy of a load run, in front of\n\
         # the synthetic servers of the machine that runs the bots. It holds a secret: keep it private,\n\
         # and make a new file for each run.\n\
         {}\n\
         # The proxy's own counters, for whoever reads the proxy's side (on its loopback only).\n\
         [metrics]\n\
         enabled = true\n\
         bind = \"127.0.0.1:9100\"\n",
        configuration(remote.proxy, &[lobby], threshold, secret, None)
    )
}

/// Writes `meridian.toml` in `dir` with a secret of its own, checks it with the `meridian` program
/// (`--check`), and tells what was written, without the secret.
pub fn write_config(
    dir: &Path,
    remote: &Remote,
    threshold: i32,
    meridian: &Path,
) -> Result<String, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let secret = fresh_secret()?;
    let path = dir.join("meridian.toml");
    std::fs::write(&path, generate(remote, threshold, &secret))
        .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    let run = |arg: &str| {
        std::process::Command::new(meridian)
            .arg(arg)
            .arg(&path)
            .stdin(std::process::Stdio::null())
            .output()
            .map_err(|e| format!("{}: {e}", meridian.display()))
    };
    let version = std::process::Command::new(meridian)
        .arg("--version")
        .output()
        .map_err(|e| format!("{}: {e}", meridian.display()))?;
    let checked = run("--check")?;
    let said = format!(
        "{}{}",
        String::from_utf8_lossy(&checked.stdout),
        String::from_utf8_lossy(&checked.stderr)
    )
    .replace(&secret, "<secret>");
    if !checked.status.success() {
        return Err(format!(
            "`{} --check` refused the file ({}): {}",
            meridian.display(),
            checked.status,
            said.trim()
        ));
    }
    Ok(format!(
        "wrote {}\n  the proxy listens on {}, lobby is {}, compression threshold {threshold}, offline\n  \
         checked by {} ({}): {}\n  the secret is in that file only",
        path.display(),
        remote.proxy,
        remote.server_address(0),
        meridian.display(),
        String::from_utf8_lossy(&version.stdout).trim(),
        said.trim()
    ))
}

/// What a run reads from the configuration the proxy was given: the secret, and the threshold.
/// It refuses a file that sends the lobby elsewhere than where the servers will listen.
pub fn read_config(path: &Path, remote: &Remote) -> Result<(String, i32), String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let folder = path.parent().unwrap_or(Path::new("."));
    let config = Config::from_toml_str_in(&text, folder)
        .map_err(|e| format!("{} is not a configuration: {e}", path.display()))?;
    let want = remote.server_address(0).to_string();
    match config.backends.iter().find(|b| b.name == "lobby") {
        Some(lobby) if lobby.address == want => {}
        Some(lobby) => {
            return Err(format!(
                "{} sends lobby to {}, and this run's servers listen on {want}: \
                 write the file again with the same --backend-bind and --backend-ports",
                path.display(),
                lobby.address
            ));
        }
        None => return Err(format!("{} has no backend called lobby", path.display())),
    }
    if config.listen != remote.proxy {
        eprintln!(
            "meridian-bench load: the file says the proxy listens on {}; the bots connect to {}",
            config.listen, remote.proxy
        );
    }
    Ok((config.forwarding_secret, config.compression_threshold))
}

// ---------------------------------------------------------------------------------------------
// What is measured from here

/// The time a TCP connection to the proxy takes to be made: the network between here and it, and
/// nothing of the proxy's work (the kernel answers). Made one at a time, and closed at once.
fn connect_times(address: SocketAddr) -> Summary {
    let mut times = Vec::new();
    for _ in 0..RAW_SAMPLES {
        let began = Instant::now();
        if let Ok(stream) = TcpStream::connect_timeout(&address, Duration::from_secs(3)) {
            times.push(began.elapsed());
            drop(stream);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    Summary::of(times)
}

fn micros(d: Duration) -> f64 {
    d.as_secs_f64() * 1e6
}

fn utc_ms(at: u64) -> String {
    format!("{}.{:03}Z", &crate::utc_at(at / 1000)[..19], at % 1000)
}

/// R1: `n` bots log in on the remote proxy, in a burst, and hold the connection.
pub fn idle(s: &Settings, n: usize) -> Result<Row, String> {
    let remote = s.remote.as_ref().ok_or("no remote proxy given")?;
    let world = start_world(
        s,
        Spec {
            name: "lobby",
            flavour: 1,
            traffic: Traffic::Idle,
            compression: Some(256),
            behind_proxy: true,
            transfers: None,
            bare: false,
        },
    )?;
    let raw = connect_times(remote.proxy);
    let live = Arc::new(Live::default());
    let sampler = Sampler::start(
        Watch {
            label: "R1 idle".into(),
            live: Arc::clone(&live),
            players: n,
            proxy: None,
        },
        Arc::clone(&s.stop),
        s.min_free,
    );
    let arrivals = arrive(s, "idle", n, &live, remote.proxy, "Idle", Role::Idle);
    let handles = arrivals.handles;
    let (joining_from, joined_at) = (
        arrivals.spans.first().map_or(0, |w| w.0),
        arrivals.spans.last().map_or(0, |w| w.1),
    );
    pause(s, Duration::from_secs(3));
    let held_from = Windows::now_ms();
    let (from, until) = (sampler.now(), sampler.now() + s.window.as_secs_f64());
    pause(s, s.window);
    let held_to = Windows::now_ms();
    s.windows.record("idle", n, "hold", held_from, held_to);
    let waves = wave_results(&live, &arrivals.spans, held_from);
    let window = sampler.between(from, until.min(sampler.now()));
    let (sent, answered) = (
        world.counters.keepalives_sent.load(Ordering::Relaxed),
        world.counters.keepalives_answered.load(Ordering::Relaxed),
    );
    let tripped = sampler.tripped.load(Ordering::Relaxed);
    let logins = Summary::of(live.login_times.lock().unwrap().clone());
    let logged = live.logins.load(Ordering::Relaxed);
    let (failed, dropped) = (
        live.failed.load(Ordering::Relaxed),
        live.dropped.load(Ordering::Relaxed),
    );
    let first = first_failure(&live);
    end(&live, handles);
    drop(sampler);
    drop(world);
    let row = Row::new()
        .with("scenario", "idle")
        .with("mode", "remote")
        .with("players", n)
        .with("logged_in", logged)
        .with("failed", failed)
        .with("dropped", dropped)
        .num("raw_connect_p50_us", micros(raw.p50), 0)
        .num("raw_connect_p95_us", micros(raw.p95), 0)
        .num("raw_connect_p99_us", micros(raw.p99), 0)
        .num("login_p50_ms", ms(logins.p50), 1)
        .num("login_p95_ms", ms(logins.p95), 1)
        .num("login_p99_ms", ms(logins.p99), 1)
        .num("login_max_ms", ms(logins.max), 1)
        .num("logins_per_s", first_wave_rate(&waves), 0)
        .with("keepalives_sent", sent)
        .with("keepalives_answered", answered)
        .num(
            "bench_cpu_mean_pct",
            mean(&window, |x| f64::from(x.bench_cpu)),
            0,
        )
        .with("join_start_utc", utc_ms(joining_from))
        .with("join_end_utc", utc_ms(joined_at))
        .with("window_start_utc", utc_ms(held_from))
        .with("window_end_utc", utc_ms(held_to))
        .with("window_start_ms", held_from)
        .with("window_end_ms", held_to)
        .with("proxy_answered", logged > 0 && !tripped)
        .with("guard_tripped", tripped)
        .with("first_failure", first);
    Ok(wave_columns(row, &waves, false))
}

/// Logins a second of the first wave: its bots over the time they took to be in the world.
fn first_wave_rate(waves: &[WaveResult]) -> f64 {
    waves.first().map_or(0.0, |w| {
        w.logged as f64 / (w.joined_ms.1.saturating_sub(w.joined_ms.0).max(1) as f64 / 1000.0)
    })
}

/// A name made at run time, for the columns of the waves (a few dozen in all, kept for good).
fn leak(text: String) -> &'static str {
    Box::leak(text.into_boxed_str())
}

/// The columns of each wave: its bots, failures, cuts, login times and when it arrived; and, for
/// the waves after the first, the round trips of the bots already in, before, during and after.
fn wave_columns(mut row: Row, waves: &[WaveResult], with_round_trips: bool) -> Row {
    row = row.with("waves", waves.len());
    for (i, w) in waves.iter().enumerate() {
        let k = i + 1;
        let name = |what: &str| leak(format!("w{k}_{what}"));
        row = row
            .with(name("bots"), w.bots)
            .with(name("logged_in"), w.logged)
            .with(name("failed"), w.failed)
            .with(name("dropped"), w.dropped)
            .num(name("login_p50_ms"), ms(w.logins.p50), 1)
            .num(name("login_p95_ms"), ms(w.logins.p95), 1)
            .num(name("login_p99_ms"), ms(w.logins.p99), 1)
            .num(name("login_max_ms"), ms(w.logins.max), 1)
            .with(name("join_start_utc"), utc_ms(w.joined_ms.0))
            .with(name("join_end_utc"), utc_ms(w.joined_ms.1))
            .with(name("join_start_ms"), w.joined_ms.0)
            .with(name("join_end_ms"), w.joined_ms.1);
        if k > 1 && with_round_trips {
            row = row
                .num(name("rest_rtt_p99_before_us"), micros(w.rest_before.p99), 0)
                .num(name("rest_rtt_p99_during_us"), micros(w.rest_during.p99), 0)
                .num(name("rest_rtt_p99_after_us"), micros(w.rest_after.p99), 0)
                .num(name("rest_rtt_max_during_us"), micros(w.rest_during.max), 0)
                .with(name("rest_rtt_samples_during"), w.rest_during.count);
        }
    }
    row
}

/// R2: the same `n` active bots straight to the server on this machine, then through the remote
/// proxy. What the proxy adds is the difference, and **includes the network** between the bots and
/// the proxy and between the proxy and the servers; `raw_connect_*` measures the first of those.
pub fn active(
    s: &Settings,
    direct_settings: &Settings,
    n: usize,
    compression: Compression,
) -> Result<Row, String> {
    let remote = s.remote.as_ref().ok_or("no remote proxy given")?;
    if compression.player() != remote.threshold {
        return Err(format!(
            "the proxy's configuration has a compression threshold of {}, which is not what \
             `--compression {}` means ({}): write the file again with that option",
            remote.threshold,
            compression.name(),
            compression.player()
        ));
    }
    let raw = connect_times(remote.proxy);
    let direct = {
        let world = start_world(
            s,
            Spec {
                name: "lobby",
                flavour: 1,
                traffic: active_traffic(s),
                compression: compression.server(),
                behind_proxy: false,
                transfers: None,
                bare: false,
            },
        )?;
        let mut direct = play(direct_settings, "R2 direct", world.address, n, None)?;
        direct.samples.clear();
        direct
    };
    let world = start_world(
        s,
        Spec {
            name: "lobby",
            flavour: 1,
            traffic: active_traffic(s),
            compression: compression.server(),
            behind_proxy: true,
            transfers: None,
            bare: false,
        },
    )?;
    let proxied = play(s, "R2 proxy", remote.proxy, n, None)?;
    drop(world);
    let (d, p) = (direct.round_trips, proxied.round_trips);
    let added = |a: Duration, b: Duration| micros(a) - micros(b);
    // A measuring packet crosses the network between the bots and the proxy twice, and the one
    // between the proxy and the servers twice; here both are one link, which `raw_connect` times
    // once, so about two of those round trips are the network's share of what the proxy "adds".
    let network = 2.0 * micros(raw.p50);
    // What the proxy adds is the difference with the direct phase: it means something only if that
    // phase kept all its bots.
    let valid = direct_kept_all(direct.failed, direct.dropped, direct.tripped);
    if !valid {
        eprintln!(
            "meridian-bench load: the direct phase lost bots ({} failed, {} cut off{}): the `added_*` columns \
             are left empty; first reason: {}",
            direct.failed,
            direct.dropped,
            if direct.tripped {
                ", a guard tripped"
            } else {
                ""
            },
            if direct.first_failure.is_empty() {
                "none noted"
            } else {
                &direct.first_failure
            }
        );
    }
    let row = Row::new()
        .with("scenario", "active")
        .with("mode", "remote")
        .with("players", n)
        .with("compression", compression.name())
        .with("logged_in", proxied.logged)
        // Both phases: the direct one, then the one through the proxy.
        .with("failed", proxied.failed + direct.failed)
        .with("dropped", proxied.dropped + direct.dropped)
        .with("direct_logged", direct.logged)
        .with("direct_failed", direct.failed)
        .with("direct_dropped", direct.dropped)
        .num("direct_login_p99_ms", ms(direct.logins.p99), 1)
        .with("direct_first_failure", direct.first_failure.clone())
        .with("added_valid", valid)
        .num("raw_connect_p50_us", micros(raw.p50), 0)
        .num("raw_connect_p95_us", micros(raw.p95), 0)
        .num("raw_connect_p99_us", micros(raw.p99), 0)
        .num("direct_rtt_p50_us", micros(d.p50), 0)
        .num("direct_rtt_p95_us", micros(d.p95), 0)
        .num("direct_rtt_p99_us", micros(d.p99), 0)
        .num("proxy_rtt_p50_us", micros(p.p50), 0)
        .num("proxy_rtt_p95_us", micros(p.p95), 0)
        .num("proxy_rtt_p99_us", micros(p.p99), 0)
        .num("proxy_rtt_max_us", micros(p.max), 0)
        .num("network_estimate_us", network, 0)
        .with("rtt_samples", p.count)
        .num(
            "down_mb_per_s",
            proxied.down_bytes / proxied.seconds / 1e6,
            2,
        )
        .num(
            "down_packets_per_s",
            proxied.down_packets / proxied.seconds,
            0,
        )
        .num("up_packets_per_s", proxied.up_packets / proxied.seconds, 0)
        .num("window_s", proxied.seconds, 1)
        .num("login_p50_ms", ms(proxied.logins.p50), 1)
        .num("login_p99_ms", ms(proxied.logins.p99), 1)
        .num(
            "bench_cpu_mean_pct",
            mean(&proxied.samples, |x| f64::from(x.bench_cpu)),
            0,
        )
        .num(
            "bench_rss_peak_mib",
            peak(&proxied.samples, |x| mib(x.bench_rss as f64)),
            0,
        )
        .with("window_start_utc", utc_ms(proxied.measured_ms.0))
        .with("window_end_utc", utc_ms(proxied.measured_ms.1))
        .with("window_start_ms", proxied.measured_ms.0)
        .with("window_end_ms", proxied.measured_ms.1)
        .with("proxy_answered", proxied.logged > 0 && !proxied.tripped)
        .with("guard_tripped", proxied.tripped || direct.tripped)
        .with(
            "first_failure",
            first_failures(&proxied.first_failure, &direct.first_failure),
        );
    let row = figure(row, valid, "added_p50_us", added(p.p50, d.p50));
    let row = figure(row, valid, "added_p95_us", added(p.p95, d.p95));
    let row = figure(row, valid, "added_p99_us", added(p.p99, d.p99));
    let row = figure(
        row,
        valid,
        "added_less_network_p50_us",
        added(p.p50, d.p50) - network,
    );
    Ok(wave_columns(row, &proxied.waves, true))
}

/// Whether the direct phase kept every bot: only then is the difference with the phase through the proxy
/// what the proxy adds.
pub(super) fn direct_kept_all(failed: u64, dropped: u64, tripped: bool) -> bool {
    failed == 0 && dropped == 0 && !tripped
}

/// A column of what the proxy adds: its value, or nothing when it cannot be read (the direct phase lost bots).
fn figure(row: Row, valid: bool, name: &'static str, value: f64) -> Row {
    if valid {
        row.num(name, value, 0)
    } else {
        row.with(name, "")
    }
}

/// The reasons the first bots failed, in the phase through the proxy and in the direct one.
pub(super) fn first_failures(proxied: &str, direct: &str) -> String {
    match (proxied.is_empty(), direct.is_empty()) {
        (true, true) => String::new(),
        (false, true) => proxied.to_owned(),
        (true, false) => format!("direct: {direct}"),
        (false, false) => format!("{proxied} | direct: {direct}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_direct_phase_must_keep_every_bot_for_the_added_time_to_count() {
        assert!(direct_kept_all(0, 0, false));
        assert!(!direct_kept_all(1, 0, false));
        assert!(!direct_kept_all(0, 3, false));
        assert!(!direct_kept_all(0, 0, true));
    }

    #[test]
    fn a_figure_that_cannot_be_read_is_left_empty() {
        let row = figure(figure(Row::new(), true, "a", 12.4), false, "b", 7.0);
        let table = Table { rows: vec![row] };
        assert_eq!(
            table.csv(),
            "a,b
12,
"
        );
    }

    #[test]
    fn the_reasons_of_both_phases_are_written() {
        assert_eq!(first_failures("", ""), "");
        assert_eq!(first_failures("a", ""), "a");
        assert_eq!(first_failures("", "b"), "direct: b");
        assert_eq!(first_failures("a", "b"), "a | direct: b");
    }

    fn remote() -> Remote {
        Remote {
            proxy: "192.168.1.25:25565".parse().unwrap(),
            bind: "192.168.1.18".parse().unwrap(),
            ports: (25701, 25710),
            threshold: 256,
        }
    }

    #[test]
    fn ports_are_a_range_of_at_least_two() {
        assert_eq!(parse_ports("25701-25710"), Ok((25701, 25710)));
        assert!(parse_ports("25701").is_err());
        assert!(parse_ports("25710-25701").is_err());
        assert!(parse_ports("80-90").is_err());
        assert!(parse_ports("a-b").is_err());
    }

    #[test]
    fn the_secret_is_new_each_time_and_32_hex_characters() {
        let (a, b) = (fresh_secret().unwrap(), fresh_secret().unwrap());
        assert_eq!(a.len(), 32);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }

    #[test]
    fn the_configuration_is_one_the_proxy_reads_and_a_run_reads_back() {
        let secret = "0123456789abcdef0123456789abcdef";
        let text = generate(&remote(), 256, secret);
        let config = Config::from_toml_str_in(&text, Path::new(".")).unwrap();
        assert_eq!(config.listen, remote().proxy);
        assert_eq!(config.forwarding_secret, secret);
        assert_eq!(config.compression_threshold, 256);
        assert_eq!(config.backends.len(), 1);
        assert_eq!(config.backends[0].name, "lobby");
        assert_eq!(config.backends[0].address, "192.168.1.18:25701");
        assert_eq!(config.max_pending_logins, 0);
        assert_eq!(config.max_players, 0);
        assert!(config.metrics.enabled);
        assert_eq!(config.metrics.bind.to_string(), "127.0.0.1:9100");
        // A run reads the secret and the threshold from the file, and refuses a file made for
        // other ports.
        let dir = std::env::temp_dir().join(format!("meridian-remote-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("meridian.toml");
        std::fs::write(&path, &text).unwrap();
        assert_eq!(
            read_config(&path, &remote()).unwrap(),
            (secret.to_owned(), 256)
        );
        let other = Remote {
            ports: (25801, 25810),
            ..remote()
        };
        let refused = read_config(&path, &other).unwrap_err();
        assert!(
            refused.contains("25701") && refused.contains("25801"),
            "{refused}"
        );
        assert!(!refused.contains(secret));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_windows_are_written_in_utc_and_in_milliseconds() {
        let windows = Windows::default();
        windows.record(
            "active proxy",
            100,
            "measure",
            1_762_358_400_000,
            1_762_358_430_500,
        );
        let csv = windows.table().csv();
        assert_eq!(
            csv,
            "step,players,phase,start_utc,end_utc,start_epoch_ms,end_epoch_ms,seconds\n\
             active proxy,100,measure,2025-11-05T16:00:00Z,2025-11-05T16:00:30Z,1762358400000,1762358430500,30.5\n"
        );
        assert_eq!(utc_ms(1_762_358_430_500), "2025-11-05T16:00:30.500Z");
    }
}
