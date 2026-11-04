# Operating Meridian

This page is for someone who wants to run Meridian in front of real Horizon servers on a
real network: what to install, how to configure it, how to start and stop it, what to
watch, and what to do when something fails. [TROUBLESHOOTING.md](TROUBLESHOOTING.md) maps
the log messages to causes. [CONFIG.md](CONFIG.md) lists every setting.

Every command and option named here was checked against the code (the checks are listed
at the end of the page). What the code does not do is said in "Known limits" and in the
"exists / missing" table, not left out.

## 0. Where things stand

Read this before you plan a deployment.

- **Meridian has not run on a real network yet.** It was run on one Windows machine, with
  everything on `127.0.0.1`, one scripted player at a time ([RUNNING.md](RUNNING.md)), and
  with the panel's three-server bench ([TESTING.md](TESTING.md),
  [TESTING-FORK.md](TESTING-FORK.md)). The only load figures are local ones: simulated players
  (10 to 1000) against a stand-in server over `127.0.0.1` ([BENCH.md](BENCH.md), which says
  not to quote them as the proxy's performance in production). Nothing was measured with real
  servers, a real network or real players.
- **The relay is the proven part.** Login (online, offline, hybrid), routing, forwarding to
  the servers, the console and the player limits are tested in CI and were run against a real
  Horizon server.
- **The seamless switch needs a Horizon build that is not released.** Moving a player
  between servers (`switch`, `send`, `/server <name>`, `/hub`, `drain --move`, the evacuation
  of a restarting server) uses the `player-*` requests of Horizon's control endpoint and the
  `[transfer]` module of its `server.toml`. On 2026-11-04 the Horizon pull requests that add
  them (#8700, #8709, #8716, #8730, #8735, #8742) were open, not merged. The switch is built
  and tested against fake servers; no run on real servers is recorded in the repository yet,
  and [TESTING-FORK.md](TESTING-FORK.md) is the procedure for the first, with the fork build
  it names. Check Horizon's releases before relying on it. Without
  those servers the proxy still relays players; a switch answers which step failed and the
  player stays where they are.
- **A planned restart needs the panel, or an operator typing `drain`.** The proxy notices an
  unplanned stop by the silence of the server's sidecar, which is after the server has gone.
  Horizon's own `server/stopping` notice is parsed but nothing feeds it yet (section 6).

## 1. What you run

| Program | Where | What it is | Required |
| --- | --- | --- | --- |
| `meridian` | one machine players can reach | The proxy. Players connect here | Yes |
| Horizon servers | behind the proxy, reachable from it only | The game servers (`horizon-server`) | Yes |
| `meridian-sidecar` | on each Horizon server's machine, one per server | Relays the proxy's player requests to the server's local control endpoint, and reports its state | Only for switching, balancing state and restart detection |
| `meridian-panel` | one machine | Starts, stops and watches a set of processes from a page on `127.0.0.1`. Built for the test bench, see section 9 | No |

The release archives hold `meridian` only (with the README, the licence and
`meridian.example.toml`). The sidecar and the panel are built from the sources:

```
cargo build --release -p meridian-sidecar -p meridian-panel
```

The programs are `target/release/meridian-sidecar` and `target/release/meridian-panel`
(`.exe` on Windows).

## 2. Install

From a release (when one is published), check the archive before running anything:

```
sha256sum -c SHA256SUMS --ignore-missing
gh attestation verify <archive> -R Akirabane/Meridian
```

From the sources, with Rust 1.85 or later and nothing else ([BUILD.md](BUILD.md)):

```
git clone https://github.com/Akirabane/Meridian
cd Meridian
cargo build --release
```

`meridian --version` and `meridian --help` show that the program runs.

## 3. The Horizon servers

Each Horizon server must be a server that only the proxy can reach, and that trusts what the
proxy tells it about the player. In its `server.toml`:

```toml
[server]
port = 25601
bind = "127.0.0.1"        # or a private address only the proxy can reach
online_mode = false       # the proxy checks players, not the server

[transfer]
enabled = true            # only for a server that moves players; see section 0
```

Then:

1. Put the proxy's forwarding secret in a file called `forwarding.secret` in the server's
   folder, one line, no newline at the end. It is the same text as `forwarding-secret` in the
   proxy's configuration. An empty file stops the server from starting.
2. Accept the Minecraft EULA yourself, in the server's folder. Meridian does not do it for you.
3. Start the server. Its log must say
   `Velocity player information forwarding is on (forwarding.secret)`.

A player who connects to the server directly is refused (`This server is behind a proxy: join
through the proxy.`), and so is a login signed with another secret. Keep the server's port
closed to everything but the proxy anyway: the forwarding is the second lock, not the first.

With `[transfer] enabled = true` Horizon refuses to start without the forwarding secret. The
transfer module is off by default, and while it is off every `player-*` request is answered
`transfers are off on this server`.

Horizon lets a frozen player go by itself after `max_freeze_seconds` of `[transfer]` (30 by
default). The proxy's `switch-freeze-limit-ms` (8000 by default) must stay below it on every
server that has a sidecar; the proxy warns at start when it does not.

## 4. The sidecar

The sidecar runs next to each Horizon server, on the same machine, and listens on
`127.0.0.1` by default. `horizon-server run` writes its control endpoint to
`.horizon/engine.json` in the server's folder; the sidecar reads that file again for every
request, so it keeps working after the server restarts on another port.

```
meridian-sidecar --endpoint-file <server folder>/.horizon/engine.json \
                 --listen 127.0.0.1:25580 \
                 --secret-file <path to this sidecar's secret file>
```

The options are `--endpoint-file` (required), `--listen` (default `127.0.0.1:25580`),
`--secret-file` (or the `MERIDIAN_SIDECAR_SECRET` environment variable), and
`--endpoint-timeout-ms` (default 5000). The secret is never an argument, because other users
of the machine can read a process's arguments.

The secret is a file of at least 16 bytes, **one per sidecar** (the proxy refuses two sidecars
that share an address or a secret file). It is different from the forwarding secret.

**Keep the sidecar on `127.0.0.1`.** Its exchange is signed (every request and reply carries
an HMAC over a random challenge, so nothing can be forged or replayed) but not encrypted: a
player's record, with their inventory and position, would cross the network in clear. The
sidecar therefore refuses any address other machines can reach unless you give both
`--allow-remote` and `--insecure-remote`, and warns on every connection from another machine.
That means the proxy must reach each sidecar over a network the operator trusts: a private
network, a VPN, or an SSH tunnel to each sidecar's `127.0.0.1`. TLS is not built.

## 5. The proxy

### 5.1 A minimal configuration

```toml
listen = "0.0.0.0:25565"
motd = "My network"
max-players = 200
forwarding-secret = "<the text of every server's forwarding.secret>"
default-backend = "lobby"
try = ["game1"]

[[backends]]
name = "lobby"
address = "10.0.0.11:25601"
sidecar = "10.0.0.11:25580"
sidecar-secret-file = "secrets/lobby.key"

[[backends]]
name = "game1"
address = "10.0.0.12:25601"
sidecar = "10.0.0.12:25580"
sidecar-secret-file = "secrets/game1.key"
```

Check it before running it. `meridian --check` exits 0 when the file is valid and 1 with a
message that names the setting at fault:

```
meridian --check meridian.toml
meridian meridian.toml
```

The configuration file is `meridian.toml` in the current folder when none is given. Relative
paths in it (`sidecar-secret-file`) are taken from the folder of the configuration file.

`auth-mode` is `"online"` by default: every player is checked with the session service. The
other values (`"offline"`, `"hybrid"`) let in players nobody has checked and are described in
the [README](../README.md#login-modes). Use `"offline"` only on a closed network.

The sidecar keys (`sidecar`, `sidecar-secret-file`) go in a `[[backends]]` entry, and
`sidecar-timeout-ms` at the top ([CONFIG.md](CONFIG.md)). A backend with no sidecar still takes
players, but no player can be switched to or from it.

### 5.2 Roles and balancing

A backend with a `role` (`"lobby"`, `"sublobby"` or `"game"`) and a `soft-cap` is balanced:

```toml
[balancing]
enabled = true

[[backends]]
name = "lobby-1"
address = "10.0.0.11:25601"
sidecar = "10.0.0.11:25580"
sidecar-secret-file = "secrets/lobby-1.key"
role = "lobby"
soft-cap = 40
hard-cap = 50
```

With `[balancing] enabled = true`, a login is placed among the servers of role `lobby` that the
player's route allows: the fullest one that is still below its soft cap, so that one server
fills before the next opens; a tie goes to the quickest to answer (`tie-break = "ping"`) or
the first listed (`"order"`). A server is not offered while it is past its caps, drained, not heard from for
`state-max-age-seconds` (15), or just failed a login. Once any backend has a role, one of them
must be a `"lobby"`. `/play` (enable it under `[commands]`) places a player on a server of role
`game`. [LOAD-BALANCING.md](LOAD-BALANCING.md) has the rules.

The proxy reads each balanced server's state (`players`, `capacity`, `accepts_transfers`)
through its sidecar. Horizon has not fixed the command that answers, so today the sidecar's
`status` request gets `unknown request`, the proxy logs `does not answer the state request: its
state is unknown, so it is treated as accepting players`, and the server is balanced on the
proxy's own count of players. That is a working state, not a fault.

### 5.3 Starting and stopping

Order: the Horizon servers, then their sidecars, then the proxy. Stop in the reverse order: the
proxy, the sidecars, the servers (the panel does exactly this).

Ctrl+C, or `end` in the proxy's console, stops the proxy: it stops accepting players, closes
their sessions, and waits up to five seconds. The players see the connection close; the proxy
sends them no message. The log says `stopping: no new connections are accepted`, then
`all connections are closed` and `stopped`.

The proxy handles Ctrl+C (SIGINT). It does not install a handler for SIGTERM, so a service
manager's default stop signal ends it at once without closing sessions cleanly. Under systemd,
set `KillSignal=SIGINT`.

### 5.4 The console is standard input

All operator commands (`list`, `servers`, `drain`, `reload`, `switch`, `end`...) are typed on
the proxy's standard input. There is no signal for `reload`, no administration port and no
remote console. A proxy started without a terminal keeps running but has no console (the log
does not say so at the default level). To keep one on a server, run the proxy in `tmux` or
`screen`, or under the panel, which types the commands for you, or read its standard input from
a pipe you write to.

The logs go to standard error, one line per event, with the time in UTC:
`2026-11-04T17:04:05.123Z INFO  ...`. `MERIDIAN_LOG` is `error`, `warn`, `info` (the default)
or `debug`. There is no log file or rotation: let the service manager or a redirect keep it.

### 5.5 Console commands

| Command | What it does |
| --- | --- |
| `help [command]` | Lists the commands, or explains one |
| `list` | The players online and where they are |
| `servers` | Each server, whether it answers, how many players; for a server with a role, a second line with its caps, whether the balancer offers it (open, closed, draining) and its ping |
| `drain <server> [--move]` | Stops offering a server with a role to new players. With `--move`, sends its players one at a time to other servers of the same role |
| `undrain <server>` | Offers a drained server again |
| `switch <player> <server>` | Moves a player while they stay connected |
| `send <player\|all> <server>` | The same, for one player or each player in turn |
| `kick <player> [reason]` | Disconnects a player |
| `reload` | Reads the configuration again and applies it to the next logins |
| `end` | Stops the proxy |

`switch` and `send` are the operator's own choice and ignore the balancer's caps. `reload` keeps the current configuration if the new file is invalid. `listen` cannot be changed
by a reload. A server removed from the file keeps its players until they leave.

### 5.6 Behind a load balancer

A layer 4 load balancer (HAProxy in `mode tcp`, nginx `stream`, a host's TCP balancer) in
front of the proxy hides the player's address: every connection comes from the balancer. With
the PROXY protocol the balancer writes the real address at the start of the connection.

```toml
[proxy-protocol]
mode = "required"
trusted = ["10.0.0.5"]        # the balancer's address, or the range of its addresses
header-timeout-ms = 3000
```

Rules the proxy keeps, so that a player cannot choose their address:

- The header is read **only** from `trusted` addresses. From any other address the proxy does
  not look for one, and in `required` it closes the connection at once. A direct client that
  writes a header gets nothing from it.
- `trusted` cannot be empty with `optional` or `required`, and cannot be a range of every
  address (`/0`). Do not list a range that players can be in.
- Make the balancer the only way in: firewall the proxy's port so that only the balancer can
  reach it, or `required` closes the others anyway. In `optional` a player who connects
  directly is a player, with their own address.
- Set the balancer to send the header (HAProxy: `send-proxy` or `send-proxy-v2` on the
  `server` line; nginx: `proxy_protocol on;` in the `stream` server). Version 1 and 2 are
  both read. A balancer's health check that connects and closes is closed quietly (debug
  level), and a header with no address (`LOCAL`) leaves the connection's own address.

The address in the header is the one in the proxy's log (`203.0.113.9:51234: Alex (...) joined
backend "lobby"`), the one `connections-per-minute` and the hybrid lookups count, and the
one forwarded to the servers. The Horizon servers do not see the PROXY header:
the proxy speaks to them with the forwarding of section 3.

Test it before opening: connect through the balancer, and check that the proxy's log shows
your own address and not the balancer's. Then connect straight to the proxy's port from a
machine that is not trusted and check, with `required`, that the connection is closed.

## 6. Restarting a server without losing its players

A server that stops takes its players with it. There are three ways the proxy learns of it
([RESTART.md](RESTART.md) has the design):

| How | When | Needs |
| --- | --- | --- |
| The operator types `drain <server> --move`, wait, then stops the server | Before the stop. Planned | A console, servers that take transfers |
| The panel does it: `[drain-before-stop]` in the bench file, with `move-players = true` | Before the stop. Planned | The panel starts and stops the server |
| The sidecar of the server does not answer for `silent-rounds` readings in a row | After the server is down | `[balancing] enabled` and a sidecar |

In all three cases the server is marked draining at once, so no new player is sent to it, and
it is offered again when it is back. The silence detection only drains: it does not move
anybody unless the evacuation is on.

```toml
[restart]
evacuate = false         # true: send the players of a server that is going down elsewhere
silent-rounds = 3        # readings in a row without an answer before it counts as going down
grace-seconds = 60       # a server told to stop may go on answering this long before it counts as back
```

`evacuate` is off by default. When it is on, the players of the server go one at a time (half a
second apart) to servers of the same role that the balancer picks, never to the server itself.
A player with nowhere to go stays and is named in the log. After the silence of an unplanned
stop it is too late to move anyone, since the server is already gone, so the evacuation is
useful for a planned restart through the panel or `drain --move`.

`servers` shows `draining (restarting: ...)` with the reason for a server the proxy drained
itself. A `drain` you type stays after the server is back until you `undrain` it.

## 7. Production checklist

Before the first player:

- [ ] Every Horizon server: `online_mode = false`, bound to a private address, port closed to
      everything but the proxy, `forwarding.secret` present and the log line
      `Velocity player information forwarding is on` seen.
- [ ] The forwarding secret is random, long, the same on the proxy and every server, and in no
      repository, ticket or chat. Same for each sidecar's secret file, one per sidecar, 16 bytes
      or more, readable only by the account that runs the program.
- [ ] `meridian --check meridian.toml` says valid. `max-players`, `connections-per-minute`
      (20 per address by default) and `connect-timeout-ms` set on purpose.
- [ ] `auth-mode` is `"online"` unless the network is closed, and you read what an unverified
      player is ([README](../README.md#login-modes)).
- [ ] Behind a load balancer: `[proxy-protocol]` is on with `trusted` set to the balancer's
      addresses only, the balancer sends the header, the proxy's port is closed to everyone
      else, and the proxy's log shows a test player's own address (section 5.6).
- [ ] Sidecars listen on `127.0.0.1`, and the proxy reaches them over a private network, a
      VPN or a tunnel. Never `--insecure-remote` over the internet.
- [ ] If you switch players: the Horizon build has `[transfer]` and `max_freeze_seconds` is
      above `switch-freeze-limit-ms`, on every server that has a sidecar. The proxy log has no
      `switch-freeze-limit-ms` warning at start.
- [ ] A test player joins through the proxy, lands on the default server, and `/server` lists
      the servers. `switch <player> <server>` ends in `moved from ... to ...` and the player
      keeps their inventory. Do this on the real servers before opening the network.
- [ ] A stop test: `end` on the proxy closes sessions and logs `stopped`. Stopping a server
      with a player on it disconnects the player with the server's own message and the proxy
      keeps running.
- [ ] A restart test with the evacuation you mean to use (section 6) and a spare server of the
      same role.
- [ ] Someone can type commands on the proxy's console when it is needed (section 5.4), and
      the log is kept somewhere.
- [ ] The proxy's service is restarted by something when it dies. It does not restart itself,
      and players who were on it are disconnected.
- [ ] You know how to roll back: the previous binary and the previous `meridian.toml`.

## 8. What to watch

The console and the log, and, if you turn it on, a page of counters and a health check.

### 8.1 The metrics page

Off by default. In `meridian.toml`:

```toml
[metrics]
enabled = true
bind = "127.0.0.1:9100"
```

Then `GET /metrics` gives the counters in the Prometheus text format, and `GET /healthz`
answers `200 ok` while the proxy takes players and `503 stopping` once it has been asked
to stop (Ctrl+C or `end`). It has no password and no TLS. Keep it on the loopback and let
a monitor on the same machine read it, or bind it to a private address and close the port
to everyone else. The proxy warns at start when the address can be reached from other
machines. `enabled` and `bind` are read when the proxy starts, not by `reload`. The page
costs the players nothing: the counters are atomic numbers bumped where things happen, and
the gauges are worked out when the page is read.

| Series | What it is |
| --- | --- |
| `meridian_players` | Players connected to the proxy |
| `meridian_backend_players{backend}` | Players on each server |
| `meridian_backend_soft_cap{backend}`, `meridian_backend_hard_cap{backend}` | The caps of each server that has a role |
| `meridian_backend_state{backend,state}` | 1 for the state the balancer sees: `open`, `closed`, `draining` (drained, restarting, or not accepting transfers) or `not_offered` (no recent report) |
| `meridian_connections_total`, `meridian_connections_throttled_total` | Connections accepted, and the ones closed unread for opening too many in a minute |
| `meridian_logins_total` | Players who ended on a server |
| `meridian_login_failures_total{reason}` | Logins that did not: `protocol` (the handshake or login broke the protocol, or the connection failed), `duplicate`, `full`, `no_route`, `unreachable`. A refusal by the authentication, and a server list ping, are not counted here |
| `meridian_transfers_total` | Transfers that succeeded |
| `meridian_transfer_failures_total{step}` | Transfers that failed, by step: `check`, `prepare`, `freeze`, `hand_over`, `commit` |
| `meridian_transfer_duration_seconds{phase}` | A histogram (buckets from 0.05 s to 10 s) of the phases of a successful transfer: `prepare`, `freeze`, `hand_over`, `commit`, and `total` (which includes reaching the sidecars) |
| `meridian_transfer_alerts_total` | The `ALERT` of the log: a source refused to let the player go after the target placed them. Alert on any increase (section 10) |
| `meridian_evacuations_total`, `meridian_evacuated_players_total{result}` | Evacuations of a server that had players, and the players moved or left where they were |
| `meridian_restarts_detected_total{cause}` | Servers found to be going down: `announced`, `panel`, `unreachable` |
| `meridian_status_rounds_total`, `meridian_status_round_duration_seconds` | Rounds of questions to the sidecars (with `[balancing] enabled`), and how long the last took |

No label carries a player's name, address or unique id. The counters start at zero when
the proxy starts. A monitor that reads `/healthz` tells that the proxy is up and not
stopping; it does not tell that a server is healthy, which `meridian_backend_state` and
`servers` do.

| Look at | What it tells you |
| --- | --- |
| `servers` | Which servers answer, with what delay, how many players, and for balanced ones open / closed / draining |
| `list` | Who is online and where |
| `WARN cannot join backend "<name>" at <address>: <why>` | A login could not reach a server. The server is tried last for `backend-retry-seconds` |
| `WARN cannot read the state of backend "<name>": <why>` | The sidecar of a balanced server is silent or refuses. Repeated, the server counts as going down |
| `WARN backend "<name>" is going down (<cause>): it takes no new player` | A restart was detected |
| `ERROR ALERT <player>: transfer <id> from <a> to <b>: ...` | A switch left two servers with different files for a player. Act on it (section 10) |
| `WARN <player> stays on <server>: <why>` | The evacuation could not move someone |
| `WARN switch-freeze-limit-ms ...` at start | The proxy's limit is not below Horizon's `max_freeze_seconds` |
| `<n> connections are still open; they close when the proxy exits` | A stop did not finish in five seconds |
| `servers` showing a server that never gets players | Drained, not heard from, at its cap, or in a group for the other kind of player |

The panel (section 9) shows each process's state, processor and memory use.

## 9. The panel

`meridian-panel` is a local tool. It starts and stops the processes listed in one bench file,
shows their state, processor and memory, gives each a console, and serves its page on
`127.0.0.1:8140` only, with a token made at each start (there is no setting for another
address). It was written for the three-server test bench ([TESTING.md](TESTING.md)), but it
runs any list of `proxy`, `server` and `sidecar` processes:

```
meridian-panel init --dir BENCH --horizon-server PATH --meridian PATH [--resources PATH] [--sidecar PATH]
meridian-panel accept-eula --dir BENCH --i-accept-the-minecraft-eula
meridian-panel --check BENCH/bench.toml
meridian-panel BENCH/bench.toml
```

`init` writes a bench on one machine (everything on `127.0.0.1`, offline login). It is not a
production layout: edit the files it writes, or write your own `bench.toml`.

What it does for operation is the drain before a stop. In `bench.toml`:

```toml
[drain-before-stop]
enabled = true
grace-seconds = 5        # how long to wait after telling the proxy
move-players = true      # also send the players elsewhere (drain <server> --move)
proxy = "proxy"          # the proxy process to tell, when the bench has several
```

When you stop or restart a server from the page, the panel types `drain <server>` (or `drain
<server> --move`) on the proxy's console, waits the grace, stops the server, and when it
answers again types `undrain <server>`, but only if the drain was its own. A server that dies
by itself is drained at once and offered again when it is back. "Stop all" announces nothing,
since the proxy stops with the rest.

The panel manages processes on the machine it runs on. It does not manage servers on other
machines, and it has no users or logins: anyone who can open its page on that machine can use
it (the page carries the token of the run).

## 10. When a switch fails

A switch has five steps, named in the error: `check` (before anything is asked of a server),
`prepare` (the target holds the player and sends their world; the client sees nothing),
`freeze` (the source freezes the player and gives the record), `hand over` (the target takes
the record, the client gets the change), `commit` (the source lets the player go). The message
ends with where the player is:

```
Alex could not move from lobby to game1: <step>: <why> (the player stays where they were)
```

| Where the player is | What it means | What to do |
| --- | --- | --- |
| `(the player stays where they were)` | Nothing changed for them. The source unfroze them, the target dropped its copy | Read the reason (TROUBLESHOOTING.md). Nothing to repair |
| `(the player was disconnected and stays where they were)` | The client had the target's world but the target did not place the player, or the freeze left was too short. The proxy disconnected them; the source keeps them | Tell them to reconnect. Read the reason |
| `(the player left during the switch)` | They disconnected while it ran. Once the record was out of the source it was imported as a normal save | Nothing, unless they report missing items: compare both servers' files |
| the `ALERT` line | The target placed the player, then the source refused to let go (the freeze ran out, or the player died on it). They are on the target; the source keeps and saves its own copy | See below |

For the `ALERT`: it carries a transfer id that both servers' logs show on every `player-*`
line (`hand-over <id>`). Find it in the two logs, then decide which `players/data/<uuid>.dat`
(and advancements and stats) to keep. The target's is the right one unless the player died on
the source, or was changed there by a command, in the freeze's last seconds. Nothing is lost,
and nothing is merged. [STATE-TRANSFER.md](STATE-TRANSFER.md) section 6.2.1 has the cases.

If a switch is given up because an answer to the export was lost, the source may have frozen
the player: they cannot move until the server's `max_freeze_seconds` passes. It lets them go by
itself.

If switches fail in a way the table does not explain, set `MERIDIAN_LOG=debug`, repeat one
switch, and keep the proxy's log and both servers' logs.

## 11. What exists and what is missing for production

| Need | Exists | Missing |
| --- | --- | --- |
| Encryption between the player and the proxy | The game's own encryption of the login in online mode (`auth-mode = "online"`). The Minecraft Java protocol has no TLS, so there is no certificate to configure | Nothing to add on this leg |
| Encryption between the proxy and a server | A signed login (the forwarding secret). After the login the leg is plain TCP: the proxy refuses a server that asks the player to authenticate | Encryption. Keep servers on a private network or a VPN |
| TLS for the sidecar | A signed, replay-proof exchange, listening on `127.0.0.1` by default; `--allow-remote` with `--insecure-remote` is refused without both | TLS. The proxy must reach each sidecar over a trusted network or a tunnel |
| The real address of a player behind a TCP load balancer (PROXY protocol) | `[proxy-protocol]`: versions 1 and 2, read only from the balancers you list, `optional` or `required`. The address is used for the log, the connection limits and the address forwarded to the servers (section 5.6) | Nothing for the PROXY protocol itself. The servers behind the proxy still see the proxy; the player's address reaches them in the forwarded data |
| Metrics | `list` and `servers` on the console, the log, and (when `[metrics]` is on) a Prometheus text page and a health check (section 8.1) | Authentication and TLS on that page, a push to a monitoring service, per-player or per-packet figures, and the relay's latency and bytes |
| Operating without a terminal | The panel types commands for a proxy it starts | A remote console or administration port, a signal for `reload`, a handler for SIGTERM |
| Logs | One line per event on standard error, in UTC | A log file, rotation, a structured format. Let the service manager keep them |
| Service files and packages | `meridian` for Linux and Windows x86_64 in release archives, with a checksum file and an attestation | A service unit, a container image or a package for the proxy, the sidecar or the panel. The sidecar and the panel are built from the sources |
| Several proxies, or a proxy that restarts without dropping players | Nothing | A second proxy for the same servers: the counts and the caps are per proxy, and players on a proxy that dies are dropped |
| Placing players | Caps, a drain, balancing among lobbies and `/play` for games, evacuation of a restarting server | Moving a group from a lobby to a game, matchmaking, a decision on who starts a move |
| Flood protection | `connections-per-minute` per address, `max-players`, `[auth] lookups-per-minute` for the hybrid mode | A firewall or a DDoS service in front of the port; the proxy is not one |
| Player management | `kick`, `max-players`, `on-duplicate`, the server's own whitelist and bans on each server | A ban list, permissions or a player database in the proxy (permissions are planned) |
| Seamless moves between servers | `switch`, `send`, `/server`, `/hub`, `/lobby`, `drain --move` against servers with the transfer module | That module in a released Horizon, and a recorded run on real servers |
| Early warning of a restart | `drain --move`, the panel's `[drain-before-stop]`, and detection by the sidecar's silence | Horizon's `server/stopping` notice fed to the detector (needs the sidecar to follow the management protocol) |
| Report of a server's state | The sidecar's `status` request and the proxy's reading of `players`, `capacity`, `accepts_transfers` | A Horizon command that answers it. Today balancing counts the proxy's own sessions |

## 12. Known limits

- Nothing has run on a real network. The load figures in [BENCH.md](BENCH.md) are local, with a
  stand-in server; plan your own test with your players' numbers before opening.
- The seamless switch needs a Horizon build with the transfer module, which was not merged on
  2026-11-04 (section 0). No run of it on real servers is recorded yet.
- Movement through the proxy, many players and long sessions were not tried against a real
  server. The hybrid mode's name lookup and the online mode's account check have not been
  tried against the real services in a recorded run.
- A server that stops without warning disconnects its players. They are not sent to the
  fallback list, which applies at login only. Only a planned restart moves them.
- Horizon's `server/stopping` notice and the panel's announcement are the signals for an early
  warning; the first is not fed (it needs the sidecar to listen to the management protocol),
  and the second needs the panel to supervise the server.
- The server's own report of its state (`accepts_transfers`, `players`, `capacity`) is not
  defined by Horizon yet; balancing counts the proxy's own sessions.
- The sidecar has no TLS, no encryption, and the proxy has no way to check one other than a
  private network.
- Signed chat is not supported across the proxy: servers behind it must not enforce secure
  chat.
- The proxy shows its own server-list message, not the server's. `/server` is not completed by
  Tab.
- Nothing handles SIGTERM or SIGHUP. The console is standard input (section 5.4).
- A proxy that dies drops its players, and nothing runs a second proxy for them.

## 13. How this page was checked

The options and commands named here were searched in the code (a missing one is a mistake in
this page, please report it):

- `meridian` options `--check`, `--help`, `--version` and `MERIDIAN_LOG`: `crates/meridian-proxy/src/args.rs`,
  `log.rs`.
- Console commands: `crates/meridian-proxy/src/console.rs` (`Commands::builtin`).
- `meridian-sidecar` options and `MERIDIAN_SIDECAR_SECRET`: `crates/meridian-sidecar/src/options.rs`.
- `meridian-panel` commands and `[drain-before-stop]`: `crates/meridian-panel/src/main.rs`, `bench.rs`.
- Configuration keys: `crates/meridian-config/src`, and [CONFIG.md](CONFIG.md).
- The metrics page, its series and its labels (section 8.1): `crates/meridian-proxy/src/metrics.rs`.
- Log and error messages: the strings in `crates/meridian-proxy/src`, `meridian-net/src`,
  `meridian-handoff/src` and `meridian-sidecar/src`.
- Horizon's side (`forwarding.secret`, the log line): Horizon's `docs/hosting.md`, "Behind a
  Velocity proxy". The transfer module: [STATE-TRANSFER.md](STATE-TRANSFER.md) and the open
  pull requests named in section 0.
