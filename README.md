# Meridian

Meridian is a proxy for [Horizon](https://github.com/horizon-rust/horizon), the Rust
reimplementation of Minecraft Java Edition, written in Rust. Players connect to the
proxy, and the proxy sends each of them to one of several Horizon servers behind it.

The goal is a proxy that does what proxies such as Velocity or BungeeCord do, built
for Horizon only, and that can move a player from one server to another without a
loading screen. Today Meridian is a working relay with routing, login modes, load
balancing and a console; the seamless switch is built and tested against fake servers,
and needs a Horizon build that has the transfer module (merged into Horizon's main on
2026-11-05, not yet run against a real server). The tables below say where each part
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
| Waiting for a place when the network is full | Done (first level), off by default | `[queue]`: a player who finds `max-players` or every lobby at its cap waits for a place (`wait-seconds`, at most 20), first come first served, instead of being refused ([docs/OPERATING.md](docs/OPERATING.md), section 5.10). The client is told nothing while it waits, and no real client was tried. Keeping the player in the configuration phase with a message of their place is not built |
| Connection limit per address | Done | Counted before any packet is read |
| Backend health | Done | A server that just failed is tried last for a while |
| Server list status | Done | The proxy's own message (or the one of the forced host the player typed), a favicon, player count, limit and a sample of names |
| Console | Done | `list`, `servers`, `drain [--move]`, `undrain`, `switch`, `send`, `kick`, `ban`, `pardon`, `banlist`, `whitelist`, `alert`, `perm`, `queue`, `reload`, `end`, `help` |
| Metrics and health check | Done, off by default | A Prometheus text page and a health check ([docs/OPERATING.md](docs/OPERATING.md), section 8.1). Without a password or TLS: keep it on the loopback or a private address |
| Staying in step with Horizon | Built | A nightly build of Meridian against the head of Horizon opens an issue when it fails, and a script moves the pinned commit ([docs/COMPAT.md](docs/COMPAT.md)). It starts no Horizon server: it checks that the code builds and the tests pass against the new protocol, not that a real Horizon and Meridian still work together |
| Reload | Done | Without a restart; `listen` needs a restart |
| In-game commands | Done | `/server`, `/hub`, `/lobby`, `/play`; and `/send`, `/glist`, `/alert` for the players who have the permission; moving starts a seamless switch |
| Load balancing | Done | Servers with a `role` are filled in order, with caps and a drain ([docs/LOAD-BALANCING.md](docs/LOAD-BALANCING.md)). Off by default |
| Moving a player to another server | In progress | The seamless switch works against the fake servers of the tests, including a scripted player that types `/server east` through the real proxy; moves with a real client between three real servers, on one machine, are described in a public comment and were not repeated or recorded with a sheet ([docs/REAL-CLIENT-RUNS.md](docs/REAL-CLIENT-RUNS.md), section 2); none was run over a real network ([docs/TESTING-FORK.md](docs/TESTING-FORK.md) is the procedure for a recorded run). It needs a sidecar next to each server and Horizon servers with the transfer module, whose pull requests were merged into Horizon's main on 2026-11-05 (the Nightly of that day, `66f3981`, was built before the last four) |
| A server that stops while players are on it | In progress | A planned restart drains the server and can send its players elsewhere, but the move has to start BEFORE the stop: `drain <server> --move` or the panel's `[drain-before-stop]`. `[restart] evacuate` runs when the server announces its stop, and a Horizon `stop` drops its players about 78 ms after announcing it (measured on real servers), too soon for a move: the fallback takes over ([docs/RESTART.md](docs/RESTART.md), [docs/VALIDATION.md](docs/VALIDATION.md)). A server that stops without warning, or kicks a player, no longer drops them: they are moved to another server with a loading screen (`[fallback]`, [OPERATING.md](docs/OPERATING.md) section 5.11). Run against three real Horizon servers with scripted players, and with a real client over the Internet, once each for a killed server, a kick and a clean stop: the player stayed in the game and saw a brief `Reconfiguring...` screen of 0.25 to 0.35 s ([docs/REAL-CLIENT-RUNS.md](docs/REAL-CLIENT-RUNS.md), section 3) |
| Pass-through of a backend's status | Planned | The proxy shows its own |
| PROXY protocol (version 1 and 2) from a load balancer | Done | `[proxy-protocol]`, read only from the addresses you list; off by default ([docs/CONFIG.md](docs/CONFIG.md)) |
| Bans and whitelist | Done | By name, unique id, address or range; a file of their own, read at the start and at `reload` ([docs/OPERATING.md](docs/OPERATING.md), section 5.8) |
| Bans and whitelist in step with the servers' | Done, off by default | `[access.sync]`: what is banned on a server reaches the proxy, and what the console bans is sent to the servers, through the sidecars. Run against three real Horizon servers with scripted players, in both directions ([docs/VALIDATION.md](docs/VALIDATION.md), section 7): it found one defect, in `pardon <name>`, since fixed. Not run with a real client or over a network ([docs/OPERATING.md](docs/OPERATING.md), section 5.12) |
| Permissions | Done | Groups with inheritance and nodes in `permissions.toml`, managed with `perm`; the commands in game check them. In offline mode a name proves nothing ([docs/OPERATING.md](docs/OPERATING.md), section 5.9) |
| Operators of the servers | Done, off by default | `[operators]`: the proxy reads each server's operators through its sidecar and treats the operators of the server a player is on as members of a group of `permissions.toml`; it keeps no list of its own and writes nothing to the servers. Tried against stand-ins, not yet against a real Horizon ([docs/OPERATING.md](docs/OPERATING.md), section 5.9) |

### The seamless switch

| Part | Status | Notes |
| --- | --- | --- |
| Model of what the client holds ("the ghost") | Done | The `meridian-ghost` crate, used by the switch (`meridian-handoff`) |
| Rewriting of ids between servers | Done | The `meridian-rewrite` crate, used by the switch |
| Moving the player's state between servers | Built, not yet run on a real Horizon | The proxy and the sidecar ask Horizon's `player-*` requests ([docs/STATE-TRANSFER.md](docs/STATE-TRANSFER.md)); Horizon's pull requests for them (#8700, #8709, #8716, #8730, #8735, #8742) were merged into its main on 2026-11-05, and the Nightly of that day (`66f3981`) was built before the last four |
| The switch itself | Built, tested on fakes | `switch`, `send`, `/server`, `/hub`, `/lobby`. Not yet run on real servers |
| Proof that there is no loading screen | Planned | Milestones J4 and J5 of the design: two real servers with different maps |
| Extensions | Built for the admission at login, the choice of server and the permission of a command; the extension's own commands and the orders are specified | A separate program the proxy asks at login, for a proxy command and for the choice of server, over a local socket; Rust SDK only ([docs/EXTENSIONS.md](docs/EXTENSIONS.md)) |

## Measures

Load was measured with the `meridian-bench load` harness, against synthetic servers and
synthetic bots: offline players, chunks that are made-up data, no game work. The limits are
part of every figure, and each page says them first.

- [docs/BASELINE.md](docs/BASELINE.md): up to 500 bots, on one Windows machine, over the
  loopback, where the bots, the synthetic servers and the proxy share the processor. It says
  what this proxy costs here (memory, threads, processor, added latency, logins, moves,
  connections that come and go). It is not the proxy's performance in production and it is
  not a promise. Not measured: more than 500 players, runs of minutes or hours, the online and
  hybrid modes, the balancer, the queue and the fallback under load, slow clients.
- [docs/LOAD-REMOTE.md](docs/LOAD-REMOTE.md) and [docs/LOAD-LAN-250.md](docs/LOAD-LAN-250.md):
  the same bots against a proxy on another machine, once: 250 bots in two waves, a proxy on a
  small Debian laptop, across a home network whose link negotiated 100 Mbit/s. The worlds sent
  at each join very probably filled that link, so the login times are the link's. It was one run,
  not repeated, and it lists defects it did not explain (the metrics page missing its deadline
  during joins, memory not given back; the memory was a leak, since found and fixed, see
  [docs/MEMORY.md](docs/MEMORY.md)).
- [docs/LOAD-VPS-250.md](docs/LOAD-VPS-250.md): 250 bots and a real client on a proxy on a VPS. One
  valid run of 250 bots, in 5 waves of 50, with one real client in the game, on a shared four-core
  VPS in Warsaw where the proxy was held to two cores, against synthetic servers on the same
  machine. It says what this proxy cost there on 5 November 2026; it is not the proxy's limit, it
  was not repeated for the reference configuration, and the bots and the proxy shared the machine.
  What the real client saw is in [docs/REAL-CLIENT-RUNS.md](docs/REAL-CLIENT-RUNS.md).
- [docs/MEMORY.md](docs/MEMORY.md): the memory the proxy kept after its players left. A load test
  of cycles of 150 simulated players on a four-core VPS showed about 6 MiB kept for each cycle and
  600 file descriptors never given back; the cause was a reference cycle in a session, fixed in
  #119, and 13 cycles on the fixed build ended each at the same 7 descriptors and 16 to 17 MiB.
- [docs/BENCH.md](docs/BENCH.md): the older latency benchmark, over the loopback, against a
  stand-in server.

No figure comes from a real Horizon server under load (the run of VALIDATION.md measured delays,
not a load) or from several real players, and none is a comparison with another proxy. The figures
with a real client come from one player ([docs/REAL-CLIENT-RUNS.md](docs/REAL-CLIENT-RUNS.md)).

### Not verified on a real Horizon

A validation run against real Horizon servers is recorded in [docs/VALIDATION.md](docs/VALIDATION.md)
(2026-11-05): three servers of the Horizon build `02f05c1`, the proxy built from `main` in a debug
build for the fixed proxy and a release build for the rest, on one machine over the loopback, with at
most two scripted players (not a real client). It confirmed `[access.sync]` in both directions, the
sidecar's connection to the servers' management protocol, the announcement of a stop and of a
start (`server/stopping`, `server/started`), and the fallback of a player whose server stops, is
killed or kicks them. It found that a Horizon `stop` drops its players about 78 ms after announcing
it, so `[restart] evacuate` cannot move anyone ahead of it, and one defect in the proxy
(`pardon <name>` after a ban taken from a server), now fixed.

Still not verified on a real Horizon:

- the seamless switch (`switch`, `send`, `/server`, `/hub`, `drain --move`) and the state
  transfer behind it, in a recorded run: a public comment describes moves with a real client on
  one machine, not repeated ([docs/REAL-CLIENT-RUNS.md](docs/REAL-CLIENT-RUNS.md), section 2);
- anything over a real network other than the runs of
  [docs/REAL-CLIENT-RUNS.md](docs/REAL-CLIENT-RUNS.md) (one real client over the Internet), with more than two clients, with a Horizon build other than
  `02f05c1`, or with a release build of the fixed proxy;
- `[restart] evacuate = false` with a player on the server that stops, and a ban of a name a
  server cannot resolve;
- the queue, as a real client sees it while it waits;
- the commands and chat that the scripted player of the test kit types, and what a real server
  answers to them;
- the hybrid mode's name lookup and the online mode's account check, against the real services.

Besides that run, the relay with one scripted player at a time was run against a real Horizon
server on an earlier commit and not repeated since ([docs/RUNNING.md](docs/RUNNING.md), section 5).

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

### Linux binaries from CI

To try Meridian on a Linux machine that has no compiler, a workflow started by hand builds
`meridian`, `meridian-sidecar`, `meridian-panel` and `meridian-bench` in release mode on Ubuntu
22.04 and keeps them for 14 days as an artifact of the run, with `SHA256SUMS`. It makes no
release and no tag. The steps, and what the archive holds, are in
[docs/BUILD.md](docs/BUILD.md), "Linux binaries from CI".

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
[docs/COMPAT.md](docs/COMPAT.md) explains how Meridian is kept in step with Horizon: the
nightly build against Horizon's head and how to move the pinned commit.

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
| Restarts that do not drop players | `drain <server> --move`, or the panel's `[drain-before-stop]`, before the stop. `[restart] evacuate = true` does not move anyone ahead of a plain Horizon `stop` (the players are dropped 78 ms after the announcement); the fallback catches them | [OPERATING.md](docs/OPERATING.md) section 6, [RESTART.md](docs/RESTART.md) |
| To try it all on one machine | The panel's test bench, three servers and a proxy | [TESTING.md](docs/TESTING.md), [crates/meridian-panel](crates/meridian-panel/README.md) |
| To put it on a real network | The checklist | [OPERATING.md](docs/OPERATING.md) section 7 |
| To fix something | The log message, its cause and what to do | [TROUBLESHOOTING.md](docs/TROUBLESHOOTING.md) |
| To know what the proxy guards against, or to report a vulnerability | The threat model, the limit of each input, and how to report privately | [SECURITY.md](docs/SECURITY.md) |

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
| `ban <name\|uuid\|ip> [reason]`, `pardon`, `banlist`, `whitelist` | Bans and the whitelist, applied before any server is asked ([docs/OPERATING.md](docs/OPERATING.md), section 5.8) |
| `alert [server:<name>] <message>` | Shows a message in the chat of the players online, or of those on one server |
| `perm ...` | Manages the permission groups and the players in them ([docs/OPERATING.md](docs/OPERATING.md), section 5.9) |
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
| `/play` | Moves the player to a game server (off unless listed in `[commands]`) |
| `/send <player\|all\|server:name> <server>` | Moves players. Off unless listed in `[commands]`; needs the permission `meridian.send` |
| `/glist [server]` | Lists the players online, by server. Needs `meridian.glist` |
| `/alert [server:name] <message>` | Shows a message to the players online, or to those on one server. Needs `meridian.alert` |

Who may type them is set by the permissions ([docs/OPERATING.md](docs/OPERATING.md), section 5.9):
the first four stay open to everyone until a group denies them.

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
  The seamless switch was run with a real client on one machine only, as a public comment
  records ([docs/REAL-CLIENT-RUNS.md](docs/REAL-CLIENT-RUNS.md), section 2), not over a real network.
- A player whose server stops without warning, or kicks them, is moved to another server as a
  new arrival, with a loading screen and without what the lost server had not saved; this ran with a real
  client over the Internet, once each for a killed server, a kick and a clean stop
  ([docs/REAL-CLIENT-RUNS.md](docs/REAL-CLIENT-RUNS.md), section 3). Only a planned restart moves them without a loading screen
  ([docs/RESTART.md](docs/RESTART.md)).
- The console is standard input: there is no remote console. On Unix, SIGTERM stops the
  proxy and SIGHUP reloads it; on Windows only Ctrl+C stops it.
  The sidecar has no TLS, so it listens on
  `127.0.0.1` ([docs/OPERATING.md](docs/OPERATING.md), sections 11 and 12).
- The proxy has been run against a real Horizon server with one scripted player at a
  time, in a debug build ([docs/RUNNING.md](docs/RUNNING.md)). Many players have been
  tried only against synthetic servers with synthetic bots, on one machine, once across a
  home network and once on a rented server (see Measures). The ban sync, the announcement of a stop and the fallback were
  run against three real servers with scripted players ([docs/VALIDATION.md](docs/VALIDATION.md)).
  The seamless movement through the proxy against a real server was run with a real client on one
  machine, as a public comment records, and not repeated
  ([docs/REAL-CLIENT-RUNS.md](docs/REAL-CLIENT-RUNS.md)); long sessions have not been tried.
- The queue holds the login only: the player is told nothing about their place while they
  wait, and the wait is at most 20 seconds, because the client gives up after 30 seconds of
  silence ([docs/OPERATING.md](docs/OPERATING.md), section 5.10).
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
- [docs/SECURITY.md](docs/SECURITY.md): who the proxy expects to be attacked by, the limit it
  holds each input to, what it does not cover, and how to report a vulnerability.
- [docs/COMPAT.md](docs/COMPAT.md): the Horizon commit Meridian is pinned to, the nightly
  build against Horizon's head, and how to move the pin.
- [docs/BASELINE.md](docs/BASELINE.md), [docs/LOAD-REMOTE.md](docs/LOAD-REMOTE.md),
  [docs/LOAD-LAN-250.md](docs/LOAD-LAN-250.md), [docs/LOAD-VPS-250.md](docs/LOAD-VPS-250.md),
  [docs/BENCH.md](docs/BENCH.md): the load measurements and their limits (see Measures).
- [docs/REAL-CLIENT-RUNS.md](docs/REAL-CLIENT-RUNS.md): the runs made with a real Horizon client:
  one over the Internet to a proxy on a rented server with 250 bots, the fallback of a player whose
  server is killed, kicks them or stops, and earlier moves between servers on one machine.
- [docs/TESTING-FORK.md](docs/TESTING-FORK.md): the procedure for trying the seamless switch
  on real servers with the Horizon fork.
- [docs/VALIDATION.md](docs/VALIDATION.md): running the ban sync, the evacuation and the stop
  announcement against real Horizon servers, step by step, and what to expect.
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
