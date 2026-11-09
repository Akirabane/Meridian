//! A chooser of servers, the smallest useful extension for the second decision point: it keeps a player
//! away from the servers the proxy says are draining, and refuses the player when no other server is left
//! (docs/EXTENSION-GUIDE.md, section 5).
//!
//! ```text
//! cargo run -p meridian-extension --example server_chooser -- <token-file> <address>
//! ```
//!
//! The token file is the one named by `token-file` in the extension's `[[extension]]` of `meridian.toml`,
//! with `grant = ["decide.server"]`, and the address is its `address`.

use meridian_extension::decide::{Choice, ServerRequest};
use meridian_extension::handshake::Secret;
use meridian_extension::serve::Extension;
use std::process::ExitCode;

/// The servers the player may be sent to, in the order the proxy would try them, without those that are
/// draining; no server left is a refusal. Every name is one of `request.candidates`, as an order needs.
fn choose(request: &ServerRequest) -> Choice {
    let open: Vec<&str> = request
        .candidates
        .iter()
        .filter(|c| c.state.as_deref() != Some("draining"))
        .map(|c| c.name.as_str())
        .collect();
    if open.is_empty() {
        Choice::refuse("No game is running.")
    } else {
        Choice::order(open)
    }
}

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let (Some(token_file), Some(address)) = (args.next(), args.next()) else {
        eprintln!("usage: server_chooser <token-file> <address>");
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
    let extension =
        Extension::new("server-chooser", env!("CARGO_PKG_VERSION"), token).on_server(choose);
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
    use meridian_extension::decide::{Auth, Candidate, Placing, Player};

    fn request(states: &[(&str, Option<&str>)]) -> ServerRequest {
        ServerRequest {
            player: Player {
                name: "Alex".into(),
                uuid: "0a2ea4ba-bced-39d2-80e1-1ad51a0e925c".into(),
                auth: Auth::Verified,
            },
            reason: Placing::Login,
            host: None,
            lost: None,
            candidates: states
                .iter()
                .map(|(name, state)| Candidate {
                    name: (*name).into(),
                    role: None,
                    players: None,
                    soft_cap: None,
                    hard_cap: None,
                    state: state.map(str::to_owned),
                    why: None,
                    up: None,
                    report: None,
                })
                .collect(),
        }
    }

    #[test]
    fn the_draining_servers_are_left_out_and_when_none_is_left_the_player_is_refused() {
        let some = request(&[
            ("lobby", Some("open")),
            ("lobby2", Some("draining")),
            ("build", None),
        ]);
        assert_eq!(choose(&some), Choice::order(["lobby", "build"]));
        let none = request(&[("lobby", Some("draining"))]);
        assert!(matches!(choose(&none), Choice::Refuse(Some(_))));
        assert!(matches!(choose(&request(&[])), Choice::Refuse(Some(_))));
    }
}
