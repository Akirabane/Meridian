//! A stand-in for a server or the proxy, for the panel's tests:
//! `meridian-panel __fake-process [--port N] [--ignore-stop]`. It prints a line on
//! each output, opens `N` on 127.0.0.1 if asked, answers its console, and ends on
//! `stop` or `end` unless told to ignore them.
use std::io::{BufRead, Write};
use std::net::TcpListener;

pub fn run(args: &[String]) -> i32 {
    let mut port = None;
    let mut ignore_stop = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--port" => {
                port = args.get(i + 1).and_then(|p| p.parse::<u16>().ok());
                i += 1;
            }
            "--ignore-stop" => ignore_stop = true,
            _ => {}
        }
        i += 1;
    }
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
