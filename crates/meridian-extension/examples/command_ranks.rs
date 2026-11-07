//! A rank system, the smallest useful extension for the permission of a command: the names it is given are
//! administrators, who may use every command the proxy answers, and everyone else is left to the proxy's
//! own `permissions.toml` (docs/EXTENSION-GUIDE.md, section 5).
//!
//! ```text
//! cargo run -p meridian-extension --example command_ranks -- <token-file> <address> [admin-name...]
//! ```
//!
//! **Its token is an administrator's credential for the network**: whoever holds it can make the proxy
//! believe anyone is an administrator. Keep its file readable by the proxy and this program only.
//! The names are only to be trusted for a player whose `auth` is `Verified`: for an unverified player (offline
//! mode) anyone can type any name, so this example gives nothing to them.

use meridian_extension::decide::{Auth, CommandRequest, NodeAnswers};
use meridian_extension::handshake::Secret;
use meridian_extension::serve::Extension;
use std::process::ExitCode;

/// Seconds the proxy may keep what this says of a player.
const KEEP: u64 = 60;

/// An administrator (a verified player whose name was given, whatever its case) has every node the command
/// checks; for anyone else nothing is said, and the file decides.
fn decide(admins: &[String], request: &CommandRequest) -> NodeAnswers {
    let is_admin = request.player.auth == Auth::Verified
        && admins
            .iter()
            .any(|a| a.eq_ignore_ascii_case(&request.player.name));
    if is_admin {
        request
            .nodes
            .iter()
            .fold(NodeAnswers::new(), |answers, node| answers.allow(node))
            .cache_seconds(KEEP)
    } else {
        NodeAnswers::new().cache_seconds(KEEP)
    }
}

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let (Some(token_file), Some(address)) = (args.next(), args.next()) else {
        eprintln!("usage: command_ranks <token-file> <address> [admin-name...]");
        return ExitCode::FAILURE;
    };
    let admins: Vec<String> = args.collect();
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
    let extension = Extension::new("command-ranks", env!("CARGO_PKG_VERSION"), token)
        .on_command(move |request| decide(&admins, request));
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
    use meridian_extension::decide::{NodeVerdict, Player};

    fn request(name: &str, auth: Auth) -> CommandRequest {
        CommandRequest {
            player: Player {
                name: name.into(),
                uuid: "0a2ea4ba-bced-39d2-80e1-1ad51a0e925c".into(),
                auth,
            },
            server: "lobby".into(),
            command: "send".into(),
            args: vec!["all".into(), "east".into()],
            nodes: vec!["meridian.send".into()],
        }
    }

    #[test]
    fn a_verified_administrator_has_the_nodes_and_nobody_else_is_given_anything() {
        let admins = vec!["Alex".to_owned()];
        let given = decide(&admins, &request("aLEX", Auth::Verified));
        assert_eq!(given.verdict("meridian.send"), NodeVerdict::Allow);
        assert_eq!(given.seconds(), KEEP);
        for other in [
            request("Steve", Auth::Verified),
            // The same name, but anyone can type it when the account is not verified.
            request("Alex", Auth::Unverified),
        ] {
            let said = decide(&admins, &other);
            assert_eq!(said.verdict("meridian.send"), NodeVerdict::Default);
        }
    }
}
