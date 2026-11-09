//! Allow-all, the extension to start from: it asks for the three decisions (`decide.login`, `decide.server`,
//! `decide.command`), leaves every one to the proxy, and writes a line for each request it is asked
//! (docs/EXTENSION-GUIDE.md). The proxy with it behaves as without it; the proxy's tests run it to check so.
//!
//! ```text
//! cargo run -p meridian-extension --example allow_all -- <token-file> <address>
//! ```
//!
//! The token file is the one named by `token-file` in the extension's `[[extension]]` of `meridian.toml`,
//! and the address is its `address`. Port 0 takes a port the system chooses: the line the SDK writes on
//! the standard error stream when it listens says which.
//!
//! What it writes, one line on the standard output for each request: the time (milliseconds since 1970),
//! the decision, the player's name, and what the proxy said of the request that is not the player's:
//! why it places them and how many servers it offers, or the permission nodes a command checks (a node may
//! name a server of the proxy's configuration, as `meridian.server.lobby`). Never the player's address or
//! the address they typed to connect, and never the command they typed or its arguments, which are their
//! own text.

use meridian_extension::decide::{
    Choice, CommandRequest, Decision, LoginRequest, NodeAnswers, Placing, ServerRequest,
};
use meridian_extension::handshake::Secret;
use meridian_extension::serve::Extension;
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

/// Milliseconds since 1970, by the system's clock.
fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_millis())
}

/// The line written for a login.
fn login_line(request: &LoginRequest) -> String {
    format!("login player={}", request.player.name)
}

/// The line written for a choice of server.
fn server_line(request: &ServerRequest) -> String {
    let reason = match request.reason {
        Placing::Login => "login",
        Placing::Fallback => "fallback",
        Placing::Other => "other",
    };
    format!(
        "server player={} reason={reason} candidates={}",
        request.player.name,
        request.candidates.len()
    )
}

/// The line written for a command: the nodes it checks, not what was typed.
fn command_line(request: &CommandRequest) -> String {
    format!(
        "command player={} nodes={}",
        request.player.name,
        request.nodes.join(",")
    )
}

/// Writes `line` with the time and the answer, `default`.
fn write(line: &str) {
    println!("{} {line} -> default", now_ms());
}

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let (Some(token_file), Some(address)) = (args.next(), args.next()) else {
        eprintln!("usage: allow_all <token-file> <address>");
        return ExitCode::FAILURE;
    };
    let token = std::fs::read(&token_file)
        .map_err(|e| e.to_string())
        .and_then(|bytes| Secret::from_file_contents(&bytes));
    let token = match token {
        Ok(token) => token,
        Err(why) => {
            eprintln!("cannot use {token_file}: {why}");
            return ExitCode::FAILURE;
        }
    };
    // Each handler leaves the decision to the proxy: `Default`, and for a command no verdict on any
    // node, which is the same.
    let extension = Extension::new("allow-all", env!("CARGO_PKG_VERSION"), token)
        .on_login(|request| {
            write(&login_line(request));
            Decision::Default
        })
        .on_server(|request| {
            write(&server_line(request));
            Choice::Default
        })
        .on_command(|request| {
            write(&command_line(request));
            NodeAnswers::new()
        });
    match extension.serve(&address) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("cannot listen on {address}: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use meridian_extension::decide::{Auth, Candidate, Player};

    fn player() -> Player {
        Player {
            name: "Alex".into(),
            uuid: "0a2ea4ba-bced-39d2-80e1-1ad51a0e925c".into(),
            auth: Auth::Verified,
        }
    }

    #[test]
    fn a_login_line_has_the_name_and_not_the_address() {
        let request = LoginRequest {
            player: player(),
            host: Some("play.example.org".into()),
            address: Some("203.0.113.7".into()),
            connected: false,
        };
        let line = login_line(&request);
        assert_eq!(line, "login player=Alex");
    }

    #[test]
    fn a_server_line_has_the_reason_and_the_count_and_not_the_host() {
        // A server as the proxy names it with balancing off: nothing but its name.
        let candidate = |name: &str| -> Candidate {
            serde_json::from_value(serde_json::json!({ "name": name })).unwrap()
        };
        let request = ServerRequest {
            player: player(),
            reason: Placing::Fallback,
            host: Some("play.example.org".into()),
            lost: Some("lobby".into()),
            candidates: vec![candidate("east"), candidate("west")],
        };
        assert_eq!(
            server_line(&request),
            "server player=Alex reason=fallback candidates=2"
        );
    }

    #[test]
    fn a_command_line_has_the_nodes_and_not_what_was_typed() {
        let request = CommandRequest {
            player: player(),
            server: "lobby".into(),
            command: "send".into(),
            args: vec!["all".into(), "a secret message".into()],
            nodes: vec!["meridian.send".into(), "meridian.send.all".into()],
        };
        let line = command_line(&request);
        assert_eq!(
            line,
            "command player=Alex nodes=meridian.send,meridian.send.all"
        );
        assert!(!line.contains("secret"));
    }
}
