# Meridian

Meridian is a proxy for [Horizon](https://github.com/horizon-rust/horizon), the Rust
reimplementation of Minecraft Java Edition, written in Rust. Players connect to the
proxy, and the proxy sends each of them to one of several Horizon servers behind it.

The goal is a proxy that does what proxies such as Velocity or BungeeCord do, built
for Horizon only, and that can move a player from one server to another without a
loading screen. Today Meridian is a working relay with routing, login modes and a
console; the seamless switch is being built. The table below says where each part
stands.

## Status

"Done" means merged, with tests that run in CI. "In progress" means some of it is
merged but it is not usable yet. "Planned" means only designed, in
[docs/DESIGN.md](docs/DESIGN.md).

### The proxy

| Function | Status | Notes |
| --- | --- | --- |
| Relay between a player and a Horizon server | Done | Both directions, with compression and encryption on each side |
| Player information forwarding | Done | Horizon's Velocity-style forwarding with a shared secret |
| Named servers and a default server | Done | |
| Login modes: online, offline, hybrid | Done | See [Login modes](#login-modes). The hybrid lookup has not been tried against the real account service |
| Routing | Done | Forced hosts, groups of servers per kind of player, and a fallback list (`try`) at login |
| Player limit and duplicate logins | Done | `max-players`; a second login is refused or replaces the first |
| Connection limit per address | Done | Counted before any packet is read |
| Backend health | Done | A server that just failed is tried last for a while |
| Server list status | Done | The proxy's own message, player count, limit and a sample of names |
| Console | Done | `list`, `servers`, `kick`, `reload`, `end`, `help`; `send` answers but cannot move players yet |
| Reload | Done | Without a restart; `listen` needs a restart |
| In-game commands | Done | `/server`, `/hub`, `/lobby`; moving answers "transfer not available yet" |
| Moving a player to another server | In progress | Neither visible nor seamless switching is available yet |
| A server that stops while players are on it | In progress | The fallback list applies at login only; players on a stopped server are disconnected |
| Pass-through of a backend's status | Planned | The proxy shows its own |
| PROXY protocol, permissions file | Planned | |

### The seamless switch

| Part | Status | Notes |
| --- | --- | --- |
| Model of what the client holds ("the ghost") | In progress | The `meridian-ghost` crate; not wired into the proxy yet |
| Rewriting of ids between servers | In progress | The `meridian-rewrite` crate; not wired into the proxy yet |
| Moving the player's state between servers | Planned | Needs a feature in Horizon's server; see [docs/STATE-TRANSFER.md](docs/STATE-TRANSFER.md) |
| The switch itself, then with no loading screen | Planned | Milestones J4 and J5 of the design |
| Extensions, metrics | Planned | |

Nothing has been measured about load, memory or the number of players a proxy can
carry. [docs/RUNNING.md](docs/RUNNING.md) describes the one run made against a real
Horizon server, one player at a time, and its limits.

## Installing

### From a release

When releases are published, each one has an archive for Linux (x86_64) and one for
Windows (x86_64), each with the `meridian` binary, this README, the licence and the
example configuration, and a `SHA256SUMS` file. Check the download against it before
running it:

```
sha256sum -c SHA256SUMS --ignore-missing
```

Each archive also has a build provenance attestation, which says it was built by this
repository's release workflow from the tagged commit:

```
gh attestation verify <archive> -R Akirabane/Meridian
```

### From the sources

Meridian needs Rust 1.85 or later and nothing else. The Minecraft protocol comes from
Horizon's `horizon-protocol` crate at a pinned commit, which Cargo fetches.

```
git clone https://github.com/Akirabane/Meridian
cd Meridian
cargo build --release
```

The binary is `target/release/meridian` (`meridian.exe` on Windows).
[docs/BUILD.md](docs/BUILD.md) has the checks and what a build costs.

## Quick start

1. **Run a Horizon server behind the proxy.** It must be reachable by the proxy only,
   with online mode off, and with the forwarding secret in a file called
   `forwarding.secret` in its folder. Horizon's server must have the Velocity-style
   forwarding (Horizon's Nightly of 2026-11-01 or later). Accepting the Minecraft
   EULA for that server is up to whoever runs it.
2. **Write the proxy's configuration.** Copy `meridian.example.toml` to
   `meridian.toml`, set `forwarding-secret` to the same text as the server's
   `forwarding.secret`, and list the server under `[[backends]]`.
3. **Check it, then run it.**

   ```
   meridian --check meridian.toml
   meridian meridian.toml
   ```

Players join the proxy's address, not the server's. [docs/RUNNING.md](docs/RUNNING.md)
walks through the whole setup on one machine.

`MERIDIAN_LOG` sets the log level (`error`, `warn`, `info`, `debug`). Ctrl+C, or `end`
in the console, stops the proxy: it stops accepting players, closes their sessions,
and waits up to five seconds for them to end.

## Commands

### Console

Type these in the terminal the proxy runs in.

| Command | What it does |
| --- | --- |
| `help [command]` | Lists the commands, or explains one |
| `list` | Lists the players online and where they are |
| `servers` | Lists the servers, whether each one answers, and who is on it |
| `send <player\|all> <server>` | Not available yet: it says where the player would go |
| `kick <player> [reason]` | Disconnects a player, telling them why |
| `reload` | Reads the configuration file again and applies it to the next logins |
| `end` | Stops the proxy, as Ctrl+C does |

### In game

Players type these in the chat. The proxy answers them itself and never passes them to
the server; every other command goes to the server as usual. `[commands]` in the
configuration turns them on or off.

| Command | What it does |
| --- | --- |
| `/server` | Lists the servers the player may join, and says which one they are on |
| `/server <name>` | Asks to move to a server. Not available yet: the player is told so |
| `/hub`, `/lobby` | Asks to move to the hub server. Not available yet either |

## Configuration

Every setting is in [meridian.example.toml](meridian.example.toml), with comments, and
in [docs/CONFIG.md](docs/CONFIG.md), with its default and what is checked. Only the
backends and the forwarding secret have no default. A configuration that does not pass
the checks is refused with a message that names the setting at fault, and `reload`
keeps the previous one.

## Login modes

The proxy, not the servers behind it, decides whether a player's account is checked.
`auth-mode` has three values:

- `online` (the default): every player is checked with the session service, as a
  vanilla server in online mode does. Every player is *verified*.
- `offline`: nobody is checked, and every player is *unverified*. Nothing proves who
  anyone is, so this is for a closed network, such as a LAN or tests.
- `hybrid`: a name that belongs to an account must be checked, so nobody can take it
  without the account; a name that belongs to no account is let in unverified.

Letting unverified players in is a choice for whoever runs the proxy, and the default
refuses them. An unverified name proves nothing: give servers for unverified players
no privileges, and keep in mind that bans and permissions based on a name are weak for
them. Verified and unverified players get different unique ids for the same name, so
the configuration refuses a server that would hold both kinds. Horizon asks each
player for their own copy of the game, and Meridian carries no game content. Section
15 of [docs/DESIGN.md](docs/DESIGN.md) has the details.

## Known limits

- Players cannot be moved between servers yet, by command or otherwise.
- A player whose server stops is disconnected, not sent to the fallback.
- The proxy has been run against a real Horizon server with one scripted player at a
  time, in a debug build ([docs/RUNNING.md](docs/RUNNING.md)). Movement through the
  proxy, many players and long sessions have not been tried.
- The hybrid mode's name lookup and the online mode's account check have not been
  tried against the real services in a recorded run.
- Signed chat is not supported across the proxy: servers behind it should not enforce
  secure chat.
- `/server` is not offered by Tab completion.
- `listen` changes need a restart.

## Documentation

- [docs/DESIGN.md](docs/DESIGN.md): how the proxy and the seamless switch work, and
  the milestones.
- [docs/RUNNING.md](docs/RUNNING.md): running the proxy in front of a Horizon server.
- [docs/CONFIG.md](docs/CONFIG.md): every setting.
- [docs/BUILD.md](docs/BUILD.md): building and testing.
- [crates/meridian-panel](crates/meridian-panel/README.md): a local panel to try the
  proxy with three Horizon servers on one machine.
- [docs/PACKETS.md](docs/PACKETS.md), [docs/STATE-TRANSFER.md](docs/STATE-TRANSFER.md),
  [docs/TEST-RIG.md](docs/TEST-RIG.md): reference material for the switch.

## Licence

MIT. See [LICENSE](LICENSE).
