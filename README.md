# Meridian

Meridian is a proxy for [Horizon](https://github.com/horizon-rust/horizon), the Rust
reimplementation of Minecraft Java Edition, written in Rust. Players connect to the
proxy, and the proxy sends each of them to one of several Horizon servers behind it.

The goal is a proxy that does what proxies such as Velocity or BungeeCord do, built
for Horizon only, and that can move a player from one server to another without a
loading screen. Today Meridian is a working relay with routing, login modes, load
balancing and a console; the seamless switch is built and tested against fake servers,
and needs a Horizon build that is not released yet. The tables below say where each part
stands. To run it, start with the [operating guide](docs/OPERATING.md).

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
| Server list status | Done | The proxy's own message (or the one of the forced host the player typed), a favicon, player count, limit and a sample of names |
| Console | Done | `list`, `servers`, `drain [--move]`, `undrain`, `switch`, `send`, `kick`, `reload`, `end`, `help` |
| Reload | Done | Without a restart; `listen` needs a restart |
| In-game commands | Done | `/server`, `/hub`, `/lobby`; moving starts a seamless switch |
| Load balancing | Done | Servers with a `role` are filled in order, with caps and a drain ([docs/LOAD-BALANCING.md](docs/LOAD-BALANCING.md)). Off by default |
| Moving a player to another server | In progress | The seamless switch works against the fake servers of the tests; no run on real servers is recorded yet ([docs/TESTING-FORK.md](docs/TESTING-FORK.md) is the procedure for the first). It needs a sidecar next to each server and Horizon servers with the transfer module, whose pull requests were still open on 2026-11-04 |
| A server that stops while players are on it | In progress | A planned restart drains the server and can send its players elsewhere (`drain <server> --move`, the panel's `[drain-before-stop]`, `[restart] evacuate`; [docs/RESTART.md](docs/RESTART.md)). A server that stops without warning disconnects its players: the fallback list applies at login only |
| Pass-through of a backend's status | Planned | The proxy shows its own |
| PROXY protocol (version 1 and 2) from a load balancer | Done | `[proxy-protocol]`, read only from the addresses you list; off by default ([docs/CONFIG.md](docs/CONFIG.md)) |
| Permissions file | Planned | |

### The seamless switch

| Part | Status | Notes |
| --- | --- | --- |
| Model of what the client holds ("the ghost") | Done | The `meridian-ghost` crate, used by the switch (`meridian-handoff`) |
| Rewriting of ids between servers | Done | The `meridian-rewrite` crate, used by the switch |
| Moving the player's state between servers | Built, waiting on Horizon | The proxy and the sidecar ask Horizon's `player-*` requests ([docs/STATE-TRANSFER.md](docs/STATE-TRANSFER.md)); Horizon's pull requests for them were open on 2026-11-04 |
| The switch itself | Built, tested on fakes | `switch`, `send`, `/server`, `/hub`, `/lobby`. Not yet run on real servers |
| Proof that there is no loading screen | Planned | Milestones J4 and J5 of the design: two real servers with different maps |
| Metrics | Built | A Prometheus text page and a health check, off by default ([docs/OPERATING.md](docs/OPERATING.md), section 8.1) |
| Extensions | Planned | |

Nothing has been measured about load, memory or the number of players a proxy can
carry. [docs/RUNNING.md](docs/RUNNING.md) describes the one run made against a real
Horizon server, one player at a time, and its limits.

## Installing

### From a release

When releases are published, each one has an archive for Linux (x86_64) and one for
Windows (x86_64), each with the `meridian` binary, this README, the licence and the
example configuration, and a `SHA256SUMS` file. The sidecar (`meridian-sidecar`) and the
panel (`meridian-panel`) are not in the archives: build them from the sources
(`cargo build --release -p meridian-sidecar -p meridian-panel`). Check the download against it before
running it:

```
sha256sum -c SHA256SUMS --ignore-missing
```

Each archive also has a build provenance attestation, which says it was built by this
repository's release workflow from the tagged commit:

```
gh attestation verify <archive> -R Akirabane/Meridian
```

### As a service or a container

[deploy/meridian.service](deploy/meridian.service) is a systemd unit (a dedicated user, a restart
after a crash, `systemctl reload` for SIGHUP), and the [Dockerfile](Dockerfile) builds a small image
that runs as a non-root user and reads its configuration from a volume. The steps are in
[docs/OPERATING.md](docs/OPERATING.md), section 5.7. Neither has been run on a real host yet.

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

That is the relay: one proxy, one or more servers, nothing else to run. The other parts
are added one at a time:

| You want | You add | Read |
| --- | --- | --- |
| Players moved between servers | A `meridian-sidecar` next to each server, `sidecar` and `sidecar-secret-file` in each `[[backends]]`, and `[transfer] enabled = true` on Horizon servers that have the transfer module | [OPERATING.md](docs/OPERATING.md) sections 3 and 4 |
| Players placed across several lobbies | `role` and `soft-cap` on the backends, and `[balancing] enabled = true` | [OPERATING.md](docs/OPERATING.md) section 5.2, [LOAD-BALANCING.md](docs/LOAD-BALANCING.md) |
| Restarts that do not drop players | `drain <server> --move`, or the panel's `[drain-before-stop]`, or `[restart] evacuate = true` | [OPERATING.md](docs/OPERATING.md) section 6, [RESTART.md](docs/RESTART.md) |
| To try it all on one machine | The panel's test bench, three servers and a proxy | [TESTING.md](docs/TESTING.md), [crates/meridian-panel](crates/meridian-panel/README.md) |
| To put it on a real network | The checklist | [OPERATING.md](docs/OPERATING.md) section 7 |
| To fix something | The log message, its cause and what to do | [TROUBLESHOOTING.md](docs/TROUBLESHOOTING.md) |

`MERIDIAN_LOG` sets the log level (`error`, `warn`, `info`, `debug`). Ctrl+C, `end`
in the console, or SIGTERM on Unix, stops the proxy: it stops accepting players, closes their sessions,
and waits up to five seconds for them to end.

## Commands

### Console

Type these in the terminal the proxy runs in.

| Command | What it does |
| --- | --- |
| `help [command]` | Lists the commands, or explains one |
| `list` | Lists the players online and where they are |
| `servers` | Lists the servers, whether each one answers, and who is on it. A server with a `role` has a second line: its players against its caps, whether the balancer offers it (open, closed, draining), and its ping ([docs/LOAD-BALANCING.md](docs/LOAD-BALANCING.md)) |
| `drain <server> [--move]` | Stops offering a server that has a `role` to new players; its players stay, or with `--move` are sent one at a time to other servers of the same role |
| `undrain <server>` | Offers a drained server again |
| `switch <player> <server>` | Moves a player to another server while they stay connected. The answer gives the time and bytes it took, or the step that failed and where the player is; the player is told too |
| `send <player\|all> <server>` | The same, for one player or, with `all`, for each player in turn |
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
| `/server <name>` | Moves the player to a server. They are told when it starts and how it ended |
| `/hub`, `/lobby` | Moves the player to the hub server |

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

- Moving players between servers needs Horizon servers with the transfer module and a
  sidecar for each; without them a move answers which step failed and the player stays.
  The switch has not been tried on a real network.
- A player whose server stops without warning is disconnected, not sent to the fallback.
  Only a planned restart moves them ([docs/RESTART.md](docs/RESTART.md)).
- The console is standard input: there is no remote console. On Unix, SIGTERM stops the
  proxy and SIGHUP reloads it; on Windows only Ctrl+C stops it.
  The sidecar has no TLS, so it listens on
  `127.0.0.1` ([docs/OPERATING.md](docs/OPERATING.md), sections 11 and 12).
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

- [docs/OPERATING.md](docs/OPERATING.md): running Meridian on a real network: what to
  install, how to configure it, the production checklist, what to watch, limits.
- [docs/TROUBLESHOOTING.md](docs/TROUBLESHOOTING.md): log messages, their cause and what
  to do.
- [docs/DESIGN.md](docs/DESIGN.md): how the proxy and the seamless switch work, and
  the milestones.
- [docs/RUNNING.md](docs/RUNNING.md): running the proxy in front of a Horizon server.
- [docs/TESTING.md](docs/TESTING.md): trying the proxy with three Horizon servers and
  the panel, step by step, on Windows.
- [docs/CONFIG.md](docs/CONFIG.md): every setting.
- [docs/LOAD-BALANCING.md](docs/LOAD-BALANCING.md): placing players across servers by
  role, the caps and the drain.
- [docs/RESTART.md](docs/RESTART.md): detecting that a server is about to restart, and
  sending its players elsewhere.
- [docs/PARITY.md](docs/PARITY.md): what Velocity and BungeeCord do, function by function, where
  Meridian stands, and what to build next.
- [docs/BUILD.md](docs/BUILD.md): building and testing.
- [crates/meridian-panel](crates/meridian-panel/README.md): a local panel to try the
  proxy with three Horizon servers on one machine.
- [crates/meridian-sidecar](crates/meridian-sidecar/README.md): the program that runs
  next to each server.
- [docs/PACKETS.md](docs/PACKETS.md), [docs/STATE-TRANSFER.md](docs/STATE-TRANSFER.md),
  [docs/TEST-RIG.md](docs/TEST-RIG.md): reference material for the switch.

## Licence

MIT. See [LICENSE](LICENSE).
