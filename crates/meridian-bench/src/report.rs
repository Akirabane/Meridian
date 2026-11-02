//! The text of a run's report: how it was made, on what, and what it measured.
use crate::run::{Load, Phase};
use crate::stats::{delta_micros, delta_millis, micros};
use std::fmt::Write;
use std::time::Duration;

/// The warning every report starts with.
pub const WARNING: &str = "Local measurements, on a shared machine, with no outside comparison. \
Do not quote them as the proxy's performance in production.";

/// What the report says about the machine and the program.
pub struct Context {
    pub command: String,
    pub date: String,
    pub machine: String,
    pub commit: String,
    pub build: &'static str,
}

/// One step of the run: the same load directly and through the proxy.
pub struct Step {
    pub load: Load,
    pub direct: Phase,
    pub proxied: Phase,
    /// The proxy's resident memory before any player.
    pub proxy_idle_memory: Option<u64>,
    /// Lines of the proxy's log at warning level or worse.
    pub proxy_warnings: usize,
    /// Whether the proxy process was gone after the step.
    pub proxy_stopped: bool,
    /// Logins the backend completed: directly, and through the proxy.
    pub backend_logins: [u64; 2],
}

pub fn header(context: &Context, load: &Load, steps: &[usize]) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "# meridian-bench report\n");
    let _ = writeln!(out, "**{WARNING}**\n");
    let _ = writeln!(out, "- Command: `{}`", context.command);
    let _ = writeln!(out, "- Date: {}", context.date);
    let _ = writeln!(out, "- Machine: {}", context.machine);
    let _ = writeln!(
        out,
        "- Commit: {} ({} build)",
        context.commit, context.build
    );
    let _ = writeln!(
        out,
        "- Load: {} players in steps, each sending {} measuring packets a second of {} bytes \
         for {} s once all are logged in",
        steps
            .iter()
            .map(usize::to_string)
            .collect::<Vec<_>>()
            .join(", "),
        load.rate,
        load.size,
        load.window.as_secs_f64()
    );
    let _ = writeln!(out, "\n## Method\n");
    let _ = writeln!(
        out,
        "Everything runs on this machine over 127.0.0.1. A stand-in backend logs each player in \
         as a Horizon server does and sends every measuring packet straight back. For each step \
         the same players send the same packets at the same rate twice: first directly to the \
         backend, then through the proxy, the real `meridian` program run as a child process. \
         A player sends one packet, waits for its echo and times it, then waits for its next \
         turn. The added latency is the difference between the two distributions of round \
         trips, both shown. CPU is each process's own time over the measured window, in percent \
         of one core; Windows counts it in scheduler ticks of about 15.6 ms, so over a window \
         of W seconds a figure under 1.56 / W % of a core reads as 0. The proxy's memory is its \
         resident size. All the players of a step connect at once, so the login figures are \
         those of a burst of logins, not of players arriving one by one. The players and the \
         backend run \
         in this program, on the same cores as the proxy: their load is shown, and what the \
         rest of the machine did is the difference."
    );
    let _ = writeln!(
        out,
        "\nNo other proxy was measured, and no figure from elsewhere is used: nothing here is a \
         comparison with another proxy."
    );
    out
}

pub fn step(step: &Step) -> String {
    let (d, p) = (&step.direct, &step.proxied);
    let players = step.load.players.max(1) as f64;
    let mut out = String::new();
    let _ = writeln!(out, "\n## {} players\n", step.load.players);
    let _ = writeln!(out, "| | Direct | Through the proxy | Added |");
    let _ = writeln!(out, "| --- | --- | --- | --- |");
    let _ = writeln!(
        out,
        "| Logged in | {} of {} | {} of {} | |",
        d.logged_in, step.load.players, p.logged_in, step.load.players
    );
    let _ = writeln!(
        out,
        "| Logins the backend completed | {} | {} | |",
        step.backend_logins[0], step.backend_logins[1]
    );
    let _ = writeln!(
        out,
        "| Logins a second | {:.0} | {:.0} | |",
        rate(d.logged_in, d.logins_took),
        rate(p.logged_in, p.logins_took)
    );
    for (name, a, b) in [
        ("Login, median", d.login.p50, p.login.p50),
        ("Login, p95", d.login.p95, p.login.p95),
    ] {
        let _ = writeln!(
            out,
            "| {name} | {} | {} | {} |",
            millis(a),
            millis(b),
            delta_millis(b, a)
        );
    }
    for (name, a, b) in [
        ("Round trip, median", d.round_trip.p50, p.round_trip.p50),
        ("Round trip, p95", d.round_trip.p95, p.round_trip.p95),
        ("Round trip, p99", d.round_trip.p99, p.round_trip.p99),
        ("Round trip, max", d.round_trip.max, p.round_trip.max),
    ] {
        let _ = writeln!(
            out,
            "| {name} | {} | {} | {} |",
            micros(a),
            micros(b),
            delta_micros(b, a)
        );
    }
    let _ = writeln!(
        out,
        "| Round trips measured | {} | {} | |",
        d.round_trip.count, p.round_trip.count
    );
    let _ = writeln!(
        out,
        "| Packets a second, each way | {:.0} | {:.0} | |",
        per_second(d.packets, d.window),
        per_second(p.packets, p.window)
    );
    let _ = writeln!(
        out,
        "| Bytes a second, each way | {:.0} | {:.0} | |",
        per_second(d.bytes, d.window),
        per_second(p.bytes, p.window)
    );
    let _ = writeln!(
        out,
        "| This program's CPU (players and backend) | {:.1} % | {:.1} % | |",
        d.bench_cpu, p.bench_cpu
    );
    let _ = writeln!(
        out,
        "| Rest of the machine's CPU | {:.1} % | {:.1} % | |",
        rest(d),
        rest(p)
    );
    if let Some(cpu) = p.proxy_cpu {
        let _ = writeln!(
            out,
            "| Proxy CPU | | {cpu:.1} % ({:.3} % a player) | |",
            f64::from(cpu) / players
        );
    }
    if let (Some(idle), Some(loaded)) = (step.proxy_idle_memory, p.proxy_memory) {
        let _ = writeln!(
            out,
            "| Proxy memory | | {} idle, {} loaded ({:.0} KiB a player) | |",
            mebibytes(idle),
            mebibytes(loaded),
            (loaded.saturating_sub(idle)) as f64 / 1024.0 / players
        );
    }
    let failed: Vec<String> = d.failures.iter().chain(&p.failures).cloned().collect();
    if d.failed + p.failed > 0 {
        let _ = writeln!(
            out,
            "\nFailures: {} direct, {} through the proxy; out of ports or descriptors: {}. \
             First: {}",
            d.failed,
            p.failed,
            d.exhausted + p.exhausted,
            failed.join("; ")
        );
    }
    let _ = writeln!(
        out,
        "\nProxy log lines at warning level or worse: {}. The proxy process was {} after the \
         step.",
        step.proxy_warnings,
        if step.proxy_stopped {
            "gone"
        } else {
            "STILL RUNNING"
        }
    );
    out
}

/// CPU used by neither this program nor the proxy, in percent of one core.
fn rest(phase: &Phase) -> f32 {
    (phase.machine_cpu - phase.bench_cpu - phase.proxy_cpu.unwrap_or(0.0)).max(0.0)
}

fn rate(count: usize, took: Duration) -> f64 {
    count as f64 / took.as_secs_f64().max(1e-9)
}

fn per_second(count: u64, window: Duration) -> f64 {
    count as f64 / window.as_secs_f64().max(1e-9)
}

fn millis(d: Duration) -> String {
    format!("{:.2} ms", d.as_secs_f64() * 1e3)
}

fn mebibytes(bytes: u64) -> String {
    format!("{:.1} MiB", bytes as f64 / 1024.0 / 1024.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stats::Summary;

    fn load() -> Load {
        Load {
            players: 10,
            window: Duration::from_secs(20),
            rate: 20,
            size: 64,
        }
    }

    #[test]
    fn a_report_starts_with_the_warning_and_says_how_it_was_made() {
        let context = Context {
            command: "meridian-bench --players 10".into(),
            date: "2026-11-03T05:00:00Z".into(),
            machine: "a test machine".into(),
            commit: "abc1234".into(),
            build: "release",
        };
        let text = header(&context, &load(), &[10, 100]);
        let first_lines: Vec<&str> = text.lines().take(3).collect();
        assert!(first_lines[2].contains(WARNING), "{text}");
        for needle in [
            "meridian-bench --players 10",
            "abc1234 (release build)",
            "10, 100 players",
            "20 measuring packets a second of 64 bytes",
            "No other proxy was measured",
        ] {
            assert!(text.contains(needle), "{needle} missing from {text}");
        }
    }

    #[test]
    fn a_step_shows_both_distributions_and_the_difference() {
        let mut direct = Phase {
            logged_in: 10,
            packets: 4_000,
            window: Duration::from_secs(20),
            ..Phase::default()
        };
        direct.round_trip = Summary {
            count: 4_000,
            p50: Duration::from_micros(80),
            p95: Duration::from_micros(150),
            p99: Duration::from_micros(300),
            max: Duration::from_micros(900),
        };
        let mut proxied = direct.clone();
        proxied.round_trip.p50 = Duration::from_micros(200);
        proxied.proxy_cpu = Some(5.0);
        proxied.proxy_memory = Some(30 * 1024 * 1024);
        let text = step(&Step {
            load: load(),
            direct,
            proxied,
            proxy_idle_memory: Some(20 * 1024 * 1024),
            proxy_warnings: 0,
            proxy_stopped: true,
            backend_logins: [10, 10],
        });
        assert!(
            text.contains("| Round trip, median | 80.0 µs | 200.0 µs | +120.0 µs |"),
            "{text}"
        );
        assert!(
            text.contains("| Packets a second, each way | 200 | 200 | |"),
            "{text}"
        );
        assert!(text.contains("5.0 % (0.500 % a player)"), "{text}");
        assert!(
            text.contains("20.0 MiB idle, 30.0 MiB loaded (1024 KiB a player)"),
            "{text}"
        );
        assert!(text.contains("The proxy process was gone"), "{text}");
    }
}
