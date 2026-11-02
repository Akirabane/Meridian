//! The `meridian-panel` program.
use meridian_panel::{bench::Bench, fake, http, init, supervisor::Supervisor};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

const USAGE: &str = "\
Usage:
  meridian-panel [BENCH]              Runs the panel for the bench file BENCH
                                      (bench.toml by default) on 127.0.0.1
  meridian-panel init --dir DIR --horizon-server PATH --meridian PATH [--resources DIR]
                                      Writes a bench of a proxy and three servers in DIR
  meridian-panel accept-eula --dir DIR --i-accept-the-minecraft-eula
                                      Accepts the Minecraft EULA for the servers of DIR,
                                      for you to run yourself after reading it
  meridian-panel --check [BENCH]      Reads and checks the bench file, then exits
  meridian-panel -h | --help";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("-h" | "--help") => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        Some("__fake-process") => ExitCode::from(fake::run(&args[1..]) as u8),
        Some("init") => report(parse_init(&args[1..]).and_then(|o| init::init(&o))),
        Some("accept-eula") => report(parse_accept(&args[1..])),
        Some("--check") => {
            let path = bench_path(args.get(1));
            report(
                Bench::load(&path)
                    .map(|b| {
                        format!(
                            "{}: valid, {} in the bench",
                            path.display(),
                            match b.processes.len() {
                                1 => "1 process".to_owned(),
                                n => format!("{n} processes"),
                            }
                        )
                    })
                    .map_err(|e| e.to_string()),
            )
        }
        Some(flag) if flag.starts_with('-') => {
            eprintln!("meridian-panel: unknown option {flag}\n\n{USAGE}");
            ExitCode::from(2)
        }
        other => run(&bench_path(other.map(|s| s.to_owned()).as_ref())),
    }
}

fn bench_path(arg: Option<&String>) -> PathBuf {
    arg.map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("bench.toml"))
}

fn report(result: Result<String, String>) -> ExitCode {
    match result {
        Ok(text) => {
            println!("{text}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("meridian-panel: {e}");
            ExitCode::FAILURE
        }
    }
}

/// The value after `--name` in `args`.
fn value(args: &[String], name: &str) -> Option<PathBuf> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from)
}

fn parse_init(args: &[String]) -> Result<init::InitOptions, String> {
    let need =
        |name: &str| value(args, name).ok_or_else(|| format!("init needs {name}\n\n{USAGE}"));
    Ok(init::InitOptions {
        dir: need("--dir")?,
        horizon_server: need("--horizon-server")?,
        meridian: need("--meridian")?,
        resources: value(args, "--resources"),
    })
}

fn parse_accept(args: &[String]) -> Result<String, String> {
    let dir = value(args, "--dir").ok_or_else(|| format!("accept-eula needs --dir\n\n{USAGE}"))?;
    init::accept_eula(&dir, args.iter().any(|a| a == init::ACCEPT_FLAG))
}

fn run(path: &Path) -> ExitCode {
    let bench = match Bench::load(path) {
        Ok(bench) => bench,
        Err(e) => {
            eprintln!("meridian-panel: {e}");
            return ExitCode::FAILURE;
        }
    };
    let listener = match http::bind(bench.panel_port) {
        Ok(listener) => listener,
        Err(e) => {
            eprintln!(
                "meridian-panel: cannot listen on 127.0.0.1:{}: {e}",
                bench.panel_port
            );
            return ExitCode::FAILURE;
        }
    };
    let supervisor = Supervisor::new(&bench);
    let panel = Arc::new(http::Panel {
        supervisor: Arc::clone(&supervisor),
        token: init::random_hex(32),
        port: bench.panel_port,
    });
    let count = match bench.processes.len() {
        1 => "1 process".to_owned(),
        n => format!("{n} processes"),
    };
    println!(
        "Meridian bench panel on http://127.0.0.1:{}/ ({count}, none started).\n\
         Ctrl+C in this terminal ends the panel, and reaches the processes it started \
         too, which stop as they do on Ctrl+C.",
        bench.panel_port
    );
    let stop = AtomicBool::new(false);
    let result = http::serve(listener, panel, &stop);
    supervisor.kill_all();
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("meridian-panel: {e}");
            ExitCode::FAILURE
        }
    }
}
