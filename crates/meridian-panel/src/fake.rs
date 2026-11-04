//! A stand-in for a server or the proxy, for the panel's tests:
//! `meridian-panel __fake-process [--port N] [--ignore-stop] [--child] [--hold]`.
//! It prints a line on each output, opens `N` on 127.0.0.1 if asked, answers its
//! console, and ends on `stop` or `end` unless told to ignore them. With
//! `--child` it first starts a child of its own, as a server starts its engine,
//! and writes its id and the child's to `fake.pid` and `child.pid` in its
//! folder. With `--hold` it only waits, a minute at most, as an engine would,
//! whatever its other arguments (such as `--control-socket PATH`). Its console also
//! answers `drain SERVER [--move]` and `undrain SERVER` in the words of the proxy's.
use std::io::{BufRead, Write};
use std::net::TcpListener;

pub fn run(args: &[String]) -> i32 {
    let mut port = None;
    let mut ignore_stop = false;
    let mut with_child = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--port" => {
                port = args.get(i + 1).and_then(|p| p.parse::<u16>().ok());
                i += 1;
            }
            "--ignore-stop" => ignore_stop = true,
            "--child" => with_child = true,
            "--hold" => {
                std::thread::sleep(std::time::Duration::from_secs(60));
                return 0;
            }
            _ => {}
        }
        i += 1;
    }
    // Kept, not waited for: the child outlives this process unless something
    // ends it.
    let _child = with_child.then(|| {
        let child = std::env::current_exe()
            .and_then(|me| {
                std::process::Command::new(me)
                    .args(["__fake-process", "--hold"])
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn()
            })
            .expect("the child starts");
        let _ = std::fs::write("fake.pid", std::process::id().to_string());
        let _ = std::fs::write("child.pid", child.id().to_string());
        child
    });
    println!("fake process ready");
    eprintln!("fake process warning on stderr");
    // Kept open until the process ends.
    let _listener = port.map(|port| {
        let listener = TcpListener::bind(("127.0.0.1", port)).expect("the port is free");
        std::thread::spawn({
            let listener = listener.try_clone().expect("a listener clones");
            move || for _ in listener.incoming() {}
        });
        listener
    });
    let mut drained: Vec<String> = Vec::new();
    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let mut out = std::io::stdout().lock();
        let words: Vec<&str> = line.split_whitespace().collect();
        match words.as_slice() {
            ["stop" | "end"] if !ignore_stop => {
                let _ = writeln!(out, "stopping");
                return 0;
            }
            ["crash"] => return 3,
            // As the proxy's console answers them (crates/meridian-proxy/src/console.rs).
            ["drain", server] | ["drain", server, "--move"] => {
                let moving = words.len() == 3;
                let newly = !drained.iter().any(|d| d == server);
                if newly {
                    drained.push((*server).to_owned());
                }
                let first = match (newly, moving) {
                    (true, false) => format!(
                        "{server} takes no new player; its players stay. undrain {server} offers it again"
                    ),
                    (true, true) => format!(
                        "{server} takes no new player; its players are being sent to other servers of its role"
                    ),
                    (false, _) => format!("{server} is already drained"),
                };
                let _ = writeln!(out, "{first}");
                if moving {
                    let _ = writeln!(out, "{server}: 0 moved, 0 stayed");
                }
            }
            ["undrain", server] => {
                if let Some(at) = drained.iter().position(|d| d == server) {
                    drained.remove(at);
                    let _ = writeln!(out, "{server} is offered to new players again");
                } else {
                    let _ = writeln!(out, "{server} is not drained");
                }
            }
            ["slow"] => {
                // An answer that comes late, like the proxy's servers, which waits
                // for every server before it prints, then prints it at once.
                std::thread::sleep(std::time::Duration::from_millis(1500));
                let _ = out.write_all(b"slow: first part\nslow: second part\n");
            }
            ["list"] => {
                // At once, as the proxy prints a command's lines.
                let _ = out.write_all(b"2 players online:\n  Alex on lobby\n  Steve on game1\n");
            }
            // As the proxy's console answers a switch: the line of a player who
            // moved, with its times, or the step at which it failed. Steve's
            // switch fails, so the panel's test sees both.
            ["send" | "switch", "Steve", target] => {
                let _ = writeln!(
                    out,
                    "Steve could not move from game1 to {target}: check: game1 has no sidecar \
                     in the configuration, so no player can be switched to or from it"
                );
            }
            ["send" | "switch", player, target] => {
                let _ = writeln!(
                    out,
                    "{player} moved from lobby to {target} in 412 ms (prepare 230 ms, freeze 61 ms, \
                     hand over 74 ms, commit 47 ms): 1890 packets, 1210344 bytes (441 chunks, 23 entities)"
                );
            }
            _ => {
                let _ = writeln!(out, "got: {line}");
            }
        }
        let _ = out.flush();
    }
    0
}
