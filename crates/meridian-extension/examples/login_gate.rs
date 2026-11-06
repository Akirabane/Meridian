//! A login gate, the smallest useful extension: it refuses the players whose names it is given and leaves
//! every other login to the proxy's own checks (docs/EXTENSION-GUIDE.md).
//!
//! ```text
//! cargo run -p meridian-extension --example login_gate -- <token-file> <address> [name...]
//! ```
//!
//! The token file is the one named by `token-file` in the extension's `[[extension]]` of `meridian.toml`,
//! and the address is its `address`.

use meridian_extension::decide::{Decision, LoginRequest};
use meridian_extension::handshake::Secret;
use meridian_extension::serve::Extension;
use std::process::ExitCode;

/// What to say of a login: refuse a player whose name is one of `blocked`, whatever its case, and have no
/// opinion of the others, which the proxy's own checks go on to.
fn decide(blocked: &[String], request: &LoginRequest) -> Decision {
    let name = &request.player.name;
    if blocked.iter().any(|b| b.eq_ignore_ascii_case(name)) {
        Decision::deny("This account is not welcome here.")
    } else {
        Decision::Default
    }
}

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let (Some(token_file), Some(address)) = (args.next(), args.next()) else {
        eprintln!("usage: login_gate <token-file> <address> [name...]");
        return ExitCode::FAILURE;
    };
    let blocked: Vec<String> = args.collect();
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
    let extension = Extension::new("login-gate", env!("CARGO_PKG_VERSION"), token)
        .on_login(move |request| decide(&blocked, request));
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
    use meridian_extension::decide::{Auth, Player};

    fn request(name: &str) -> LoginRequest {
        LoginRequest {
            player: Player {
                name: name.into(),
                uuid: "0a2ea4ba-bced-39d2-80e1-1ad51a0e925c".into(),
                auth: Auth::Verified,
            },
            host: None,
            address: None,
            connected: false,
        }
    }

    #[test]
    fn a_blocked_name_is_refused_whatever_its_case_and_the_others_are_left_alone() {
        let blocked = vec!["Alex".to_owned()];
        assert!(matches!(
            decide(&blocked, &request("aLEX")),
            Decision::Deny(Some(_))
        ));
        assert_eq!(decide(&blocked, &request("Steve")), Decision::Default);
        assert_eq!(decide(&[], &request("Alex")), Decision::Default);
    }
}
