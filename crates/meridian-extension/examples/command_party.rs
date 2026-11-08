//! A command of the extension's own, the smallest one that keeps something: `/party invite <player>`,
//! `/party list` and `/party leave`, with `/p` as an alias (docs/EXTENSIONS.md, 4.3). The proxy takes the
//! command from the player, never passes it to the server, and shows the lines this program answers.
//!
//! ```text
//! cargo run -p meridian-extension --example command_party -- <token-file> <address>
//! ```
//!
//! It is a demonstration: a party is kept in memory, an invitation is a join with no acceptance, and the
//! program asks for nothing but the `commands` capability, so it can say something to the player who typed
//! the command and nothing more. A name typed by a player whose `auth` is not `Verified` proves nothing, as
//! everywhere.

use meridian_extension::handshake::Secret;
use meridian_extension::run::{Lines, RunRequest};
use meridian_extension::serve::Extension;
use meridian_extension::wire::CommandSpec;
use std::collections::HashMap;
use std::process::ExitCode;
use std::sync::Mutex;

/// Who is in whose party: the leader's name to the members', in order.
#[derive(Default)]
struct Parties(Mutex<HashMap<String, Vec<String>>>);

impl Parties {
    fn run(&self, request: &RunRequest) -> Lines {
        let me = request.player.name.clone();
        let mut parties = self.0.lock().unwrap_or_else(|e| e.into_inner());
        match request.args.as_slice() {
            [word, who] if word == "invite" && *who != me => {
                let party = parties
                    .entry(me.clone())
                    .or_insert_with(|| vec![me.clone()]);
                if !party.contains(who) {
                    party.push(who.clone());
                }
                Lines::new([
                    format!("{who} is invited to your party."),
                    format!("Your party: {}.", party.join(", ")),
                ])
            }
            [word] if word == "list" => match parties.get(&me) {
                Some(party) => Lines::one(&format!("Your party: {}.", party.join(", "))),
                None => Lines::one("You lead no party."),
            },
            [word] if word == "leave" => match parties.remove(&me) {
                Some(_) => Lines::one("Your party is over."),
                None => Lines::one("You lead no party."),
            },
            _ => Lines::one("Usage: /party <invite <player>|list|leave>"),
        }
    }
}

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let (Some(token_file), Some(address)) = (args.next(), args.next()) else {
        eprintln!("usage: command_party <token-file> <address>");
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
    let parties = Parties::default();
    let party = CommandSpec {
        name: "party".into(),
        aliases: vec!["p".into()],
        usage: "/party <invite <player>|list|leave>".into(),
    };
    let extension = Extension::new("command-party", env!("CARGO_PKG_VERSION"), token)
        .command(party, move |request| parties.run(request));
    match extension.serve(&address) {
        Ok(()) => ExitCode::SUCCESS,
        Err(why) => {
            eprintln!("cannot listen on {address}: {why}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use meridian_extension::decide::{Auth, Player};

    fn typed(name: &str, args: &[&str]) -> RunRequest {
        RunRequest {
            player: Player {
                name: name.into(),
                uuid: "0a2ea4ba-bced-39d2-80e1-1ad51a0e925c".into(),
                auth: Auth::Verified,
            },
            server: "lobby".into(),
            command: "party".into(),
            args: args.iter().map(|a| (*a).to_owned()).collect(),
        }
    }

    fn said(parties: &Parties, name: &str, args: &[&str]) -> Vec<String> {
        parties.run(&typed(name, args)).as_slice().to_vec()
    }

    #[test]
    fn a_party_is_led_by_who_invites_and_ends_when_they_leave_it() {
        let parties = Parties::default();
        assert_eq!(said(&parties, "Alex", &["list"]), ["You lead no party."]);
        assert_eq!(
            said(&parties, "Alex", &["invite", "Bob"]),
            ["Bob is invited to your party.", "Your party: Alex, Bob."]
        );
        // Inviting twice does not add twice.
        let again = said(&parties, "Alex", &["invite", "Bob"]);
        assert_eq!(again[1], "Your party: Alex, Bob.");
        assert_eq!(
            said(&parties, "Alex", &["list"]),
            ["Your party: Alex, Bob."]
        );
        // Another player leads no party of theirs.
        assert_eq!(said(&parties, "Bob", &["list"]), ["You lead no party."]);
        assert_eq!(said(&parties, "Alex", &["leave"]), ["Your party is over."]);
        assert_eq!(said(&parties, "Alex", &["list"]), ["You lead no party."]);
    }

    #[test]
    fn what_is_not_a_command_of_the_party_gets_the_usage() {
        let parties = Parties::default();
        for args in [
            &[][..],
            &["dance"],
            &["invite"],
            &["invite", "Alex"],
            &["list", "x"],
        ] {
            assert_eq!(
                said(&parties, "Alex", args),
                ["Usage: /party <invite <player>|list|leave>"],
                "{args:?}"
            );
        }
    }
}
