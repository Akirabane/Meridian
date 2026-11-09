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

- **Meridian has run on a real network only in a few runs, each with its limits.** It was run
  on one Windows machine, with everything on `127.0.0.1`, one scripted player at a time
  ([RUNNING.md](RUNNING.md)), and with the panel's three-server bench ([TESTING.md](TESTING.md),
  [TESTING-FORK.md](TESTING-FORK.md)). Beyond that, 250 simulated players were run against a proxy
  on a laptop across a home network ([LOAD-LAN-250.md](LOAD-LAN-250.md)), and one real client
  was connected over the Internet to a proxy on a rented VPS, with 250
  simulated players on the same machine, for five minutes ([LOAD-VPS-250.md](LOAD-VPS-250.md),
  [REAL-CLIENT-RUNS.md](REAL-CLIENT-RUNS.md)), and then in three fallback runs. Those are one player,
  one run of each, a stock Horizon server and a shared machine. Then, on 6 and 7 November, real
  clients were moved between two real Horizon servers over a real network, and a server was stopped
  with players on it ([REAL-NETWORK-RUN.md](REAL-NETWORK-RUN.md)): one session each, three clients on the same PC as
  the second server. The load figures are those of
  simulated players against stand-in servers ([BENCH.md](BENCH.md) says not to quote them as the
  proxy's performance in production); none is a production figure.
- **The relay is the proven part.** Login (online, offline, hybrid), routing, forwarding to
  the servers, the console and the player limits are tested in CI and were run against a real
  Horizon server.
- **The seamless switch needs a Horizon build with the transfer module.** Moving a player
  between servers (`switch`, `send`, `/server <name>`, `/hub`, `drain --move`, the evacuation
  of a restarting server) uses the `player-*` requests of Horizon's control endpoint and the
  `[transfer]` module of its `server.toml`. The Horizon pull requests that add them (#8700,
  #8709, #8716, #8730, #8735, #8742) were merged into Horizon's main on 2026-11-05, the last four
  at 23:47 UTC; the Nightly of that day (`66f3981`) was built before them. The switch is built
  and tested against fake servers, and it was run with real clients between two real Horizon servers
  over a real network on 6 November 2026, with its limits ([REAL-NETWORK-RUN.md](REAL-NETWORK-RUN.md)): one session, three
  clients on the same PC as the second server, no failure injected during a move, no long run.
  Earlier moves with a real client between three real servers on one machine are described only in a
  public comment ([REAL-CLIENT-RUNS.md](REAL-CLIENT-RUNS.md), section 2).
  [TESTING-FORK.md](TESTING-FORK.md) is the procedure for a recorded
  run, with the fork build it names. Check that your Horizon build has the transfer module before
  relying on it. Without
  those servers the proxy still relays players; a switch answers which step failed and the
  player stays where they are.
- **A planned restart needs the panel, an operator typing `drain`, or the server's own announcement.**
  Without them the proxy notices an unplanned stop by the silence of the server's sidecar, which is
  after the server has gone. Horizon's own `server/stopping` notice is fed to the proxy when the
  sidecar is started with `--management-properties` and the backend has `sidecar-notices = true`
  (off by default; section 6).

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
`--endpoint-timeout-ms` (default 5000). With `--management-properties <server folder>/server.properties` the sidecar also listens to
the server's management protocol and tells the proxy when the server announces its stop (section 6;
it is off without the option; it also carries the servers' bans and whitelist for `[access.sync]`,
section 5.12). The secret is never an argument, because other users
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

The proxy reads each balanced server's state through its sidecar, with Horizon's
`transfer-status` request (`online`, `max_players`; the other fields of its answer are not
used). A server whose `online` has reached `max_players` is not offered, and a server never
gets more players than it says it can take. A server whose `[transfer]` module is off answers
that transfers are off, and is shown as `draining (it does not accept transfers)`. A Horizon
build from before `transfer-status` answers `unknown request`: the proxy then logs `does not
answer the state request: its state is unknown, so it is treated as accepting players` once,
and the server is balanced on the proxy's own count of players. That is a working state, not a
fault; with a Horizon that has `transfer-status` the line does not appear.

### 5.3 Starting and stopping

Order: the Horizon servers, then their sidecars, then the proxy. Stop in the reverse order: the
proxy, the sidecars, the servers (the panel does exactly this).

Ctrl+C, or `end` in the proxy's console, stops the proxy: it stops accepting players, closes
their sessions, and waits up to five seconds. The players see the connection close; the proxy
sends them no message. The log says `stopping: no new connections are accepted`, then
`all connections are closed` and `stopped`.

On Linux and the other Unix systems the proxy also handles two signals, which is what a service
manager sends:

| Signal | What the proxy does |
| --- | --- |
| `SIGTERM` | Stops, exactly as Ctrl+C does. This is what `systemctl stop` and `docker stop` send, so a service needs no `KillSignal=` setting. The log says `SIGTERM: stopping` first |
| `SIGHUP` | Reads the configuration file again, exactly as the console's `reload` does (section 5.4; [CONFIG.md](CONFIG.md), "What a reload changes"). The log says `SIGHUP: reloading <file>`, then one `reload: ...` line for each change. A file that does not pass the checks changes nothing, and the log says `reload: not reloaded, <file> is unchanged in use: <why>` at the warning level. |

On Windows only Ctrl+C (and `end` in the console) stops the proxy, and there is no signal for
`reload`. A second SIGTERM while the proxy is stopping does nothing; SIGKILL, which no program
can catch, ends the proxy at once and drops its players.

### 5.4 The console is standard input

All operator commands (`list`, `servers`, `drain`, `reload`, `switch`, `end`...) are typed on
the proxy's standard input. On Unix, `kill -HUP` runs `reload` (section 5.3). There is no
administration port and no remote console. A proxy started without a terminal keeps running but has no console (the log
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
| `alert [server:<name>] <message>` | Shows a message in the chat of the players online, or of those on one server |
| `perm ...` | Manages the permission groups and the players in them (section 5.9) |
| `ban <name\|uuid\|ip> [reason]` | Bans a player name, an account, an address or a range (`203.0.113.0/24`), and disconnects the players it holds (section 5.8) |
| `pardon <name\|uuid\|ip>` | Lifts a ban |
| `banlist` | Lists the bans |
| `whitelist <add\|remove> <name\|uuid>`, `whitelist list`, `whitelist <on\|off>` | Manages the whitelist (section 5.8) |
| `queue` | The players who wait for a place, in order, and for how long (section 5.10) |
| `reload` | Reads the configuration again and applies it to the next logins |
| `end` | Stops the proxy |

`switch` and `send` are the operator's own choice and ignore the balancer's caps. `reload` keeps the current configuration if the new file is invalid. `listen` cannot be changed
by a reload. A server removed from the file keeps its players until they leave.

#### 5.5.1 An extension at the console

An extension that you grant `orders.console` (`grant = ["orders.console"]`, by that name only: no pattern gives it, and no other
capability does) can run **any line of the table above**, with your rights, over its connection: the same commands, no more and no less
(`ban`, `perm`, `whitelist`, `kick`, `drain`, `send`, `reload`, `end`...). **Granting it is giving the console of the proxy**: do it
only for a program you would give the keyboard to. **It reads more than the read orders do**: `list` prints every player's address and
`servers` every server's address (and probes each server at each call), with or without `addresses`; and `end`, `perm`, `ban`, `whitelist`,
`kick` (not protected by `meridian.kick.exempt`), `drain --move` and `send all` are the most dangerous lines. It is off by default. Each line is written in the log **before** it runs, as
`extension "<name>" runs: <line>`, and what it said after, as `ran: <line>: ...`. **Every line costs half the burst of the extension's orders** (the budget limits how often, not what), and one line runs at a time for an
extension (a lot of moves started by `send` or `drain --move` can run in parallel). The proxy answers `did not finish` after four seconds (the line goes on). `end` stops the proxy; `reload` is applied after the
answer is written, because it can change the extension's own entry. See docs/EXTENSIONS.md, section 5.

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

### 5.7 Running as a service

Two files in the repository run the proxy without a terminal: a systemd unit
([deploy/meridian.service](../deploy/meridian.service)) and a container image
([Dockerfile](../Dockerfile)). Neither has been run on a real host (see the end of this section).
Under either one the proxy has **no console**: its standard input is closed, so `list`, `servers`,
`drain`, `send` and the other console commands are not available (section 5.4). `reload` is,
through SIGHUP, and the log shows what happened.

#### systemd

1. Put the binary where the unit expects it, and make a user that owns nothing else:

   ```
   install -m 0755 meridian /usr/local/bin/meridian
   useradd --system --no-create-home --shell /usr/sbin/nologin meridian
   ```

2. Put the configuration and the files it names (the sidecar secrets; a relative path is taken from
   the configuration's folder) in `/etc/meridian`, readable by that user and nobody else:

   ```
   install -d -o root -g meridian -m 0750 /etc/meridian
   install -o root -g meridian -m 0640 meridian.toml /etc/meridian/meridian.toml
   ```

3. Install the unit, check the configuration, and start it:

   ```
   install -m 0644 deploy/meridian.service /etc/systemd/system/meridian.service
   systemctl daemon-reload
   meridian --check /etc/meridian/meridian.toml
   systemctl enable --now meridian
   ```

Then:

| You want | You run | What happens |
| --- | --- | --- |
| The log | `journalctl -u meridian -f` | The proxy writes to standard error, which the journal keeps |
| A new configuration | `systemctl reload meridian` | SIGHUP: the file is read again as by `reload`; an invalid file changes nothing, and the journal says why |
| A stop | `systemctl stop meridian` | SIGTERM: sessions closed, up to five seconds; `TimeoutStopSec=15` leaves room beyond that |
| A restart after a crash | nothing | `Restart=on-failure` starts it again after five seconds. A stop you asked for is not restarted. Players on a crashed proxy are dropped (section 12) |

What the unit restricts, so that you can tell if a setting of yours needs more: it runs as the
`meridian` user with no capabilities, so the port must be above 1024 (for 25565 nothing to do; for
a lower port add `AmbientCapabilities=CAP_NET_BIND_SERVICE` and the same in
`CapabilityBoundingSet=`); the file system is read-only to it, apart from a private `/tmp`; only
TCP and local sockets; the home directories are hidden; `LimitNOFILE=65536`, since each player
takes two sockets. A configuration file or secret outside `/etc/meridian` needs
`ReadOnlyPaths=` for it.

#### Container

```
docker build -t meridian .
docker run -d --name meridian -p 25565:25565 -v /srv/meridian:/etc/meridian:ro meridian
```

The image is built in two stages (the Rust image, then a small Debian 12 "distroless" image with
the C library and no shell) and runs as the unprivileged user `nonroot` (65532). It holds the
binary and `meridian.example.toml` in `/usr/share/meridian`. The configuration is the volume:
`/etc/meridian/meridian.toml` is read, so `/srv/meridian` above must hold `meridian.toml` and the
files it names, readable by user 65532.

- **Addresses.** Inside the container `127.0.0.1` is the container. A backend or a sidecar on the
  host, or in another container, needs an address the container can reach: a name on a shared
  Docker network, the host's address, or `--network host` for a proxy on the host's network.
  `[metrics] bind` and a sidecar's address have the same rule; to scrape the metrics page, set
  `bind = "0.0.0.0:9100"` in the file and publish the port only to the machine that scrapes it,
  as the page has no authentication.
- **Stopping and reloading.** `docker stop` sends SIGTERM and waits ten seconds before it kills,
  which is enough for the five seconds the proxy takes. `docker kill --signal=HUP meridian`
  reloads the file, and `docker logs meridian` shows the log.
- **The console.** There is none unless the container is started with `-i`: `docker run -it ...`,
  or `docker attach` to a container started with `-i`, types the console commands.
- **Version of Rust.** The build argument `RUST_VERSION` (1.90 by default) names the Rust image;
  the sources need 1.85 or later: `docker build --build-arg RUST_VERSION=1.91 -t meridian .`
- **The build needs the network**, for the crates and for Horizon's protocol crate on GitHub, and
  the image is not published anywhere: the release workflow builds the archives only.

#### What was checked

The unit passes `systemd-analyze verify` (systemd 257) and scores 1.5 ("OK") in
`systemd-analyze security --offline`. It was not started on a host: nothing here shows that the
proxy runs under every one of its restrictions, so start it once and read `journalctl -u meridian`
before relying on it. The Dockerfile was not built, because no Docker engine was running where it
was written: the first `docker build` may need a change, such as the Rust version.

### 5.8 Bans and the whitelist

The proxy keeps one list of bans and one whitelist for the whole network, applied before any
server is asked for the player. They are off until you use them: with no `access.toml`, nothing
changes. What they are and where they are kept is in [CONFIG.md](CONFIG.md) (`[access]`).

| Command | Effect |
| --- | --- |
| `ban <name\|uuid\|ip> [reason]` | A name (any case), an account's unique id, an address, or a range such as `203.0.113.0/24` or `2001:db8::/32`. The players it holds who are connected are disconnected with the reason. Banning something already banned is refused |
| `pardon <name\|uuid\|ip>` | Lifts the ban of that name, id, address or range (the same range, not an address inside it) |
| `banlist` | The bans, with their reasons |
| `whitelist add <name\|uuid>`, `whitelist remove <name\|uuid>` | Changes the whitelist. It takes no address |
| `whitelist on`, `whitelist off` | Turns it on or off. While it is on only the players on it join. `whitelist list` shows its state and entries |

Each change is written to the file at once, by writing the whole file beside it and replacing
the old one. If the file cannot be written (a full disk, no right to the folder), the ban or
the entry still applies in memory and the command says it is not on disk, so that a ban is
never held back; fix the folder and make the change again, or it is lost at the next start.
The file is read at the start, where a file that cannot be read stops the proxy (running
without a ban it holds would let in who it keeps out), and at `reload` (the console command or SIGHUP, which
run the same code), where one that cannot be read changes nothing. You may edit it by hand and `reload`.

When it applies:

- An **address** is checked as the connection is taken, before a byte is read from it, like
  `connections-per-minute`. The connection is closed without a word, so a banned address is
  not told the reason. Behind a load balancer, it is the player's address from the PROXY
  header that is checked (section 5.6), never the balancer's.
- A **name** and a **unique id** are checked once the player is identified (after the account
  check in online mode), before a place under `max-players` is taken or a server is tried. The
  player is told `You are banned from this server.` and the reason, or `You are not
  whitelisted on this server.`. An address ban is checked again here, so a player who got past
  the first check is still refused, with the reason.
- A **ban** disconnects the connected players it holds when it is made, and at a `reload`. The
  **whitelist** applies to those who join: turning it on, or removing someone, does not
  disconnect a player already connected; `kick` does.
- A ban comes before the whitelist and before a second login of the same player.

What they can and cannot tell about a player:

- With `auth-mode = "online"` the name and the unique id are those of the account, checked. Ban
  and whitelist by unique id: a name changes hands, and a name entry matches whoever holds it
  when they join.
- In offline mode, and for the unverified players of the hybrid mode, **the name is whatever
  the client says it is**. A ban by name stops that name only: the player comes back under
  another. A **whitelist by name is not a barrier**: anyone who types a listed name joins as
  them. The unique id is derived from the name as typed (`Alex` and `alex` have different ids),
  so a ban by unique id stops only the spelling it was made for. Use the whitelist as a security
  measure only in online mode, and the bans by address for what you cannot stop by name.
- An address is the player's, or a shared one (a school, a mobile carrier, a VPN). A range
  that is wider than needed bans people who did nothing.

Not done: the commands in game (they wait for the permissions), a duration or an expiry, and a
record of who banned and when. The servers keep their own lists; the proxy follows them and
tells them its own only if you turn `[access.sync]` on (section 5.12). Refusals are counted
as `meridian_login_failures_total{reason="banned"}` and `{reason="not_whitelisted"}` (section
8.1).

### 5.9 Permissions and the commands in game

The console is the operator's and needs no permission. What is checked is what a **player** types
in chat. Each command has a node; a player has it, or not, through the groups they are in. What a
group is and how a node is decided is in [CONFIG.md](CONFIG.md) (`[permissions]`).

| Command in game | Node | Open to everyone before there is a rule |
| --- | --- | --- |
| `/server`, `/server <name>` | `meridian.server`, and `meridian.server.<name>` to be listed and to go to `<name>` | yes |
| `/hub`, `/lobby` | `meridian.hub` | yes |
| `/play` | `meridian.play` | yes |
| `/send <player\|all\|server:name> <server>` | `meridian.send` | no |
| `/glist [server]` | `meridian.glist` | no |
| `/alert [server:name] <message>` | `meridian.alert` | no |
| (no command) a player an extension must not kick | `meridian.kick.exempt`: an extension granted `orders.moderate` cannot kick them with `players/kick`; the console's `kick` still can | no |

So a proxy that has no `permissions.toml` behaves as before: the first four are for everyone. To
close one, deny its node in `default` (`deny = ["meridian.play"]`), and give it back to a rank by
allowing it in that group: the group the player is in is nearer than `default`, so it wins. A
server can be hidden from `/server` and refused to the players without its node
(`deny = ["meridian.server.staff"]`); `/hub` and `/play` place a player by their own rules and
do not look at `meridian.server.<name>`.

The staff commands are off until they are listed in `[commands] enabled` (a backend may have a
command of the same name). With one on, a player without the node is told `You do not have
permission to use /send.`, and the command is not passed to the server. With it:

- `/send <player|all|server:name> <server>` moves one player, every player, or those on a server,
  as the console's `send` does (the same code, and the same transfer: it needs a sidecar next to
  each server). The command answers at once, `Sending 2 players to east...`; the moves go on in the
  background, each player is told how theirs ended, and the sender is told at the end how many
  failed. The log has each move.
- `/glist [server]` lists the players online, one line for each server that has some, or the
  players of one server.
- `/alert [server:name] <message>` shows `[Alert] <message>` in the chat of every player online, or
  of those on one server (`server:east` first or last in the line). The sender is told how many
  were reached. A player who is in the loading screen or in the configuration phase of the game
  is counted but does not see it.

Each use of `/send` and `/alert` is in the log with the name of the player who typed it.

The console manages the permissions; every change is written to the file at once (the whole file
beside it, then renamed), and the file is read at the start, at `reload` and on SIGHUP, with the
same rules as the bans (section 5.8): a file that cannot be read stops the start and changes
nothing at a reload, and a change that cannot be written holds in memory and says so.

| Command | Effect |
| --- | --- |
| `perm list` | The groups, with their nodes and what they inherit, and the players in them |
| `perm list <player>` | The groups a player is in, and a second line when they are also in the operator group because they are an operator of the server they are on (section 5.9, "The operators of the servers") |
| `perm check <player> <node>` | Whether the player has the node, an online operator's group included. By name only unless the player is online or an id is given: a rank given to an id is not seen from a name |
| `perm grant <player\|uuid> <group>`, `perm revoke <player\|uuid> <group>` | Puts a player in a group, or takes them out |
| `perm group <group> create`, `perm group <group> delete` | Makes a group, or deletes it (it leaves the groups that inherit it and the players in it) |
| `perm group <group> allow <node>`, `deny <node>`, `unset <node>` | What the group says of a node or a pattern (`meridian.server.*`); `unset` makes it say nothing |
| `perm group <group> inherit <group>`, `uninherit <group>` | Makes the group take the nodes of another when it says nothing of its own. A loop is refused |

What a name proves:

- With `auth-mode = "online"` a name and an id are those of the account, checked. Rank by unique
  id: a name changes hands, and a rank given to a name goes to whoever holds it when they join.
- In offline mode, and for the unverified players of the hybrid mode, **the name is whatever the
  client says it is**: anyone who types the name of a player with a rank has it. **In offline mode
  do not give `meridian.send` or `meridian.alert` to a name**; the permissions are no barrier
  there, only a convenience on a network where every player is trusted. The same limit as the
  whitelist (section 5.8).

#### The operators of the servers

The proxy keeps no list of operators. Each Horizon server owns its own (`ops.json`, with a permission
level from 0 to 4 for each), and with `[operators] from-servers = true` the proxy **reads** them and
reflects them; it never changes them. An operator is a rank that the servers decide, not one you
write in `permissions.toml`:

```toml
# meridian.toml
[operators]
from-servers = true
group = "operator"   # the group of permissions.toml the operators are members of
min-level = 4        # the lowest permission level that counts
```

```toml
# permissions.toml: what the operators may do at the proxy
[group.operator]
allow = ["meridian.send", "meridian.glist", "meridian.alert"]
```

- **Who counts.** A player is an operator of the server they are on *now*, at `min-level` or above,
  read from that server. An operator of `east` who is on `lobby` has nothing from `east`; one who
  moves from `east` to `lobby` loses it with the move. There is no union of the servers: an operator
  of a test server would otherwise have `/send all` on the whole network. `from = ["lobby", "east"]`
  limits the servers whose operators count (the others are not even asked).
- **What it gives.** For the proxy's commands only, the player is a member of `group`, with its
  nodes, what it inherits and what it denies, as if `perm grant` had put them there. It is worked out
  for each command and never written: `permissions.toml` does not change, `perm list` does not list
  them as members, and `perm list <player>` and `perm check` say that the player is also in the group
  as an operator. A group that `permissions.toml` does not define gives nothing, and a `deny` of one of
  the player's own groups still wins a tie.
- **How it is read.** Through the sidecar of each server (started with `--management-properties`,
  with a `[[sidecars]]` entry), by the same rounds as the bans of section 5.12: the list `operators`
  is read when the proxy starts, whenever a sidecar has connected to its server again or lost
  notices, and whenever the server announces that an operator was added or removed. The list read
  **replaces** what was known of that server: an operator removed on the server is removed here, at
  the next command the player types after the sidecar has told the proxy (within a few seconds). The notice is only the
  reason to read again; its content is not trusted.
- **When a server cannot be read.** A server that has not been read has no operators: nobody is
  refused less than before. What was read from a server is believed for `stale-after-seconds`
  (600) after the last time the list was known to be right: it was read, or the sidecar was heard
  connected to the server while no read was failing. Past that, the server has no operators until
  it is read again. A deop made while the proxy could not hear thus does not last for ever. A server
  that refuses the read (Horizon without the `operators` method, or not ready yet) is tried again
  after a wait that grows, with a warning in the log; a read that fails renews nothing, so the list
  it was meant to replace ages as above even while the sidecar stays connected.
- **Reload.** A reload applies the new `[operators]` at once: with `from-servers` off, or for a server
  that no longer counts, what was read is forgotten.
- **What a name proves.** The operator is found by **unique id**. A server that lists an operator without an id is
  matched by name only for a player whose account was checked (online mode, or a verified player of the
  hybrid mode). The same limit as the rest of this section applies: in offline mode the name, and the
  id made from it, is whatever the client says it is, and that is as true for Horizon's own `/op`.
  **In offline mode do not turn this on for a network where players are not trusted.**
- **Not done.** The proxy does not use `bypassesPlayerLimit` (an operator does not pass `max-players`)
  and does not write operators to the servers (`/op` and `/deop` stay the servers' own).

Not done: in-game commands to manage the groups, a permission for the backends' own commands (the
proxy decides only what it answers), a group for a kind of login (verified or not), a rank that
expires, and a link with the servers' own operator levels.

**The commands of an extension.** An extension that you grant `commands` can have commands of its own
([EXTENSIONS.md](EXTENSIONS.md) 4.3): the proxy takes them from the player and never passes them to a server.
Each has the node `ext.<extension>.<command>` (the extension's name in `[[extension]]`, lower case), **open to
everyone** like `/hub`; to close one, deny its node in `permissions.toml`
(`[group.default] deny = ["ext.parties.party"]`). Two things to know before you grant it:

- **An extension granted `commands` can take any word that is not a command of the proxy that is enabled in
  `[commands]`, including a command of your servers** (`/msg`, `/tell`, `/login`, `/op`...): the proxy then
  never passes it to a server, the extension is told what the player typed after it, and answers what it
  likes. **The token of such an extension is an administrator's credential**; keep its file readable by the
  proxy and the extension only. The proxy has no list of words it keeps from an extension.
- **While the extension is down, its commands stay its own** (decided: the word stays reserved): the player
  who types one reads `That command did not answer.`, and the server does not see it. Before the first hello
  after the proxy starts, the word is not known yet and goes to the server.

### 5.10 Waiting for a place when the network is full

Off by default. Without it, a player who finds the proxy at `max-players`, or every lobby at its
hard cap, is refused with "The server is full". With it, they wait:

```toml
[queue]
enabled = true
wait-seconds = 15   # at most 20
max-length = 50
```

- **Order.** First come, first served. A player who arrives while others wait goes behind them,
  even if a place is free at that instant: the place is for the first in the line, who is woken
  when it is given back. Nobody is let in ahead of the line.
- **Before the line.** The connection limit, the bans, the whitelist, the login itself (online
  checks included) and the check for a second login all come first. A refused player never
  waits, and the line holds only players who logged in.
- **What wakes the line.** A session ending (a place given back), a reload that changes
  `max-players`, and the proxy stopping. Nothing polls: the thread of each waiting player sleeps
  on a condition variable. Once a second it looks up to see whether the player is still there
  (their connection is read for a millisecond), because nothing else tells it. A player who
  closes the connection leaves the line at that tick, and costs no place and no server login.
  The waiting adds no thread: it is the thread the connection already had, and the line is
  limited by `max-length`.
- **When every lobby is at its hard cap** (balancing on) the player waits as well, at the head of
  the line if they had their turn: they give the place back, and try the lobbies again at each
  event and at each tick. A lobby that has room again because it was undrained, or because a
  server came back, is seen at the next tick (within a second). While the player at the head waits
  for a lobby, those behind wait too; they would all be sent to the same lobbies.
- **How long.** `wait-seconds`, at most 20. The player's client gives up when nothing reaches it
  for 30 seconds (vanilla's read timeout), and the proxy sends it nothing while it waits. After
  the wait the proxy still has to reach a server (`connect-timeout-ms`, 5 s by default), so the
  configuration is refused if `wait-seconds` plus `connect-timeout-ms` is more than 27 s. When the
  wait runs out the player gets the refusal that was always given.
- **What the player sees.** The client waits where the login left it (no real client was tried).
  The proxy tells it nothing
  about its position, and sends it no keep-alive: this is the first level, which only holds the
  login. The packets the client sends while it waits (its settings, its brand) are kept and
  passed to the server it ends up on.
- **Look at it.** The console's `queue` lists who waits, in order, and for how long. The metrics
  page (section 8.1) has `meridian_queue_length`, `meridian_queue_entered_total`,
  `meridian_queue_refused_total` (the line was at its longest),
  `meridian_queue_left_total{cause}` (`served`, `timeout`, `gone`) and the histogram
  `meridian_queue_wait_seconds{cause}`. A login that ends in the refusal after a wait is also
  counted in `meridian_login_failures_total{reason="full"}`.
- **Reload.** `enabled`, `wait-seconds` and `max-length` apply to the players who arrive after
  it. A new `max-players` applies to those who wait too: raising it serves the line at once.
  Turning the queue off leaves the players already in it to be served or time out.

Not done: a message with the player's position, a wait longer than the client's silence allows
(that needs the connection held in the configuration phase with keep-alive packets), a line
for each group or each role, and priority for some players. It has not run against a real
client or real servers; the tests use pipes and a stand-in backend.

### 5.11 When a server is lost

A server can end a player's session without the proxy having asked for it: it crashes, the
network to it breaks, or it disconnects the player (a kick, a shutdown, a rule of its own). With
`[fallback]` on, which it is by default, the player is not disconnected: they are moved to
another server, and the chat tells them why.

```toml
[fallback]
on-kick = true
on-crash = true
max-attempts = 3
```

- **What the player sees.** The game goes back to its loading screen, as when a server changes
  their dimension, and joins the new server like a new arrival: nothing is carried over from the
  lost server, whose state is gone, so the player is where the new server puts them. Once they are
  in, a chat line says what happened: `You lost the connection to lobby. You were moved to east.`
  or, for a kick, `You were disconnected from lobby: <the server's reason>. You were moved to east.`
  A real client over the Internet saw a `Reconfiguring...` screen with a Disconnect button, then
  `Loading terrain...`, for 0.25 to 0.35 s in three runs, and stayed in the game
  ([REAL-CLIENT-RUNS.md](REAL-CLIENT-RUNS.md), section 3). The reason was shown as the proxy
  could read it: after a clean stop of a Horizon server it was its translation key,
  `multiplayer.disconnect.server_shutdown`, not translated. That is what the change below is for.
  When the server's reason is plain text (`test kick from the server console`), the line is that
  text, as above. When it is a translation key, as a clean stop sends
  (`multiplayer.disconnect.server_shutdown`), the proxy does not copy the key: it sends the line as
  a chat component that holds the server's reason, and the player's game should translate it:
  `You were disconnected from lobby: <the game's words for the key>. You were moved to east.` The
  words would be the game's, in the player's language, not the proxy's, and a game that has no
  text for the key would show the key, as before. This was tried against stand-ins, with the
  component read back by the protocol crate's own reader, and not yet with a real client.
  Only the parts of the server's component that say words are passed on: `text`, `translate` with
  its `fallback` and arguments, `extra` and `color`. The click and hover events, the insertion, the
  font, scores, selectors, key bindings and anything unknown are dropped, because the line is shown
  on the server the player has just reached and the one they left must not be able to put a link or
  a command to run in their chat. A reason that is empty, over 4 KiB, nested more than 8 levels
  or made of more than 64 parts is shown as the text the proxy could read from it.
- **Where to.** The servers a login would try for this player, in the same order and by the same
  rules: the forced host's, or the route's group, or the default server, then the `try` list; the
  servers that are down or failed lately last; and with `[balancing] enabled` the caps, the drain
  and the roles decide among the lobbies (section 5.2). Not the server that was lost, nor one that
  lost this player a moment ago.
- **No loop.** At most `max-attempts` servers are tried in a row: each server connected to counts,
  and so does each one that loses the player in turn. A server that keeps the player for a minute
  ends the run and the count starts again. If no server is left, or the count is spent, the player
  is disconnected with a message that says the server was lost and that no other could take them
  (for a kick, the server's own reason comes first, passed on the same way: a translation key is
  left for the game to translate). A kick that arrives when the session is already ending (the
  proxy ended it, or the player left, or a move is under way) is shown to the player as the server
  sent it, component included, on the disconnect screen, which runs no click; it is still limited
  to 4 KiB.
- **When it does not apply.** The proxy ended the session itself (`kick`, `ban`, a stop of the
  proxy, a second login that replaced this one); the player left; a switch of this player is under
  way (`switch`, `/server`, `drain --move`); the backend's disconnect was already passed on because
  `on-kick` is off; or the player is still in the configuration phase, at login, where the
  client cannot be taken back to its start. In those cases the session ends as it always did. A
  player who was refused or is waiting in the queue (section 5.10) is not in the game yet, and
  nothing here touches them.
- **Look at it.** Each loss is one line in the log: `lost backend "lobby" (the connection was
  lost: ...); moved to "east" in 41 ms (servers tried: 1; players on "lobby" then: 3)`, or
  `... no other server took them` as a warning. The time is from the loss being heard to the
  other server having taken the player's login; the servers tried are those the proxy asked,
  including those that did not answer; the players on the lost server are those still counted
  on it when this player's loss was handled (one that was moved before is no longer counted, so
  the first line of a crash shows the most). When the player's game follows, a second line says
  `on "east" 187 ms after the loss of "lobby": the game followed`: the time from the same start
  to the client having acknowledged and the other server's connection being the one it plays on.
  The server's configuration and world still reach the player after that. A game that never
  follows (the client does not acknowledge in time, or leaves) has no second line. The metrics page
  (section 8.1) has `meridian_fallbacks_total{cause,result}`, with `cause` `kick` or `crash` and
  `result` `moved` or `failed`.
- **Reload.** A new server list and a new `max-attempts` apply to the next loss, including that of
  a player who was already connected. `on-kick` and `on-crash` apply to the players who log in
  after the reload; the others keep what they logged in with.

Limits: the move is a new login, so it needs the servers to take a player who joins like any
other; it does not use the sidecars and does not wait for a transfer module. A player whose
server is lost takes the full configuration phase again, which a vanilla client accepts from the
game as it does when a server reconfigures it, and a real Horizon client over the Internet did so three times without leaving the game (a
killed server, a kick and a clean stop, once each, [REAL-CLIENT-RUNS.md](REAL-CLIENT-RUNS.md),
section 3); the tests use a stand-in client and servers. A player in the middle of a switch, or at login, is not
saved by it. Their inventory, position and the rest of what was unsaved on the lost server are
whatever that server had last written.

### 5.12 Keeping the bans and the whitelist in step with the servers

The proxy's bans and whitelist (section 5.8) and each Horizon server's own are separate lists. A
player banned on one server can still join another through the proxy, and a ban made at the
proxy's console leaves the servers' lists as they were. `[access.sync]` joins them, in two
directions that you turn on one by one. Both are off.

```toml
[access.sync]
from-servers = true   # what is banned, pardoned or allowed on a server reaches the proxy
to-servers = true     # what the console bans, pardons or whitelists is sent to the servers
```

Both go through the sidecars, which are already clients of the servers' management protocol
(section 4). Each sidecar must be started with `--management-properties` and have a `[[sidecars]]`
entry in the proxy's configuration; a backend without either is left out, and the proxy says so
once in its log. Nothing else of the management protocol is used: the sidecar lets through only
the three lists and, for each, a read and the addition or removal of one entry, and the read of the
operators (section 5.9, which is a separate setting).

**`from-servers`.** The proxy asks each sidecar for what its server announced about its lists,
over the same signed exchange as the other requests.

- A ban made on a server is a ban of the proxy, with the reason `banned on east` (or `banned on
  east: <the server's reason>`), by unique id when the server gives one and by name otherwise. A
  player who is connected to another server is disconnected with that reason, as for a ban at the
  console. A ban of an address is a ban of that address.
- A pardon on a server lifts the proxy's ban only if it is the one taken from that server (the
  reason says so). A ban you made at the console, or one taken from another server, stays.
- A player added to a server's allowlist is added to the proxy's whitelist. A player removed from
  an allowlist is **not** removed from the whitelist: another server may still allow them.
- When the proxy starts, and whenever a sidecar has connected to its server again (a restart of
  the server), or has lost notices, or was restarted itself, the proxy reads the three lists the
  server holds and takes what it does not have. A server that has just started refuses the first read (`Method cannot be dispatched pre server initialization: minecraft:bans`); the proxy logs a warning and tries again, and takes the lists a few seconds later (4.8 s in the run of [VALIDATION.md](VALIDATION.md)). That reading **never removes** an entry of the
  proxy, whatever the server holds.
- A ban that comes back with the source `Meridian` is the proxy's own (see "No loop" below) and
  is not taken again.

**`to-servers`.** `ban`, `pardon`, `whitelist add` and `whitelist remove` at the console are
applied to the proxy's lists first, which is what refuses the player at once, then sent to every
backend that has a sidecar: `ban <name>` becomes `bans/add` with the source `Meridian` and the
reason, `ban <ip>` becomes `ip_bans/add`, `pardon` becomes `bans/remove` or `ip_bans/remove`, and
the whitelist becomes `allowlist/add` or `allowlist/remove`. The command prints a line saying where
it is being sent.

- Each backend has a queue of its own, in the order the changes were made, so a ban and the
  pardon after it cannot cross. A server that is slow holds up no other and never the console or
  a login.
- A server that cannot be reached (down, its sidecar down, its sidecar not connected to it) is
  tried again after 2, 5, 15, 30, 60 and 120 seconds. After the sixth failure the change is given
  up for that server, with a warning in the log; the proxy's lists keep it, so the player is
  still refused at the proxy. A server that answers with an error (a name it cannot resolve) is
  not tried again, and the log says what it answered.
- A queue holds 256 changes; past that the oldest is dropped, with a warning.
- Turning `to-servers` off at a reload drops what was still waiting. The queue is in memory:
  a restart of the proxy forgets it.

**No loop.** What the console decides is what is sent. A change that comes from a server is
never sent on to the others (it is the proxy that refuses the player there, not the other
servers' lists that change). The notice a server sends back for a change the proxy made carries
the source `Meridian` and is ignored; a change already in the lists is no change.

What is not synchronised:

- **Ranges of addresses.** Horizon bans single addresses. A range banned at the proxy
  (`203.0.113.0/24`) stays at the proxy; the console says it is not sent.
- **Bans that end.** A ban on a server with an expiry is not copied, as the proxy's bans never
  end; the log says so for each. A ban you want everywhere, make it at the console.
- **Names and unique ids.** The two sides may name a player differently. A ban by name goes to a
  server as a name, which it must resolve itself (its user cache, or the profile service, or the
  offline player of an offline server); a name it cannot resolve is refused and logged. A ban by
  unique id goes as an id. In offline mode, and for the unverified players of the hybrid mode, a
  name is not proof of who is playing (section 5.8), and neither is an entry synchronised from a
  name.
- **A ban taken from a server is filed by unique id**, and `banlist` shows `uuid ...: banned on
  east`. With the sync on and the proxy in offline mode, `pardon <name>` lifts it too (the id of
  the name as typed and the id of the lower-case name are both tried, since a server may use
  either, and only an entry marked `banned on <server>` is lifted), and sends the pardon to the
  servers by name, as for any other pardon. By name, because a server removes an entry by id
  only if it knows the profile (its user cache); a ban written in its file by hand is not
  removed that way. In online and hybrid mode an account's id cannot
  be known from a name, so none is guessed: give the id that `banlist` shows (`pardon
  069a79f4-44e9-4726-a5be-fca90e38aaf5`). With the sync off, `pardon` is unchanged.
- **A whitelist entry taken from a server's allowlist is filed by unique id** too, and carries no
  mark. With the sync on and the proxy in offline mode, `whitelist remove <name>` removes the entry
  under the id of that name (as typed, or in lower case) and sends the removal to the servers by
  name; in online and hybrid mode give the id that `whitelist list` shows. An id is made from a
  spelling, so the name must be spelled as the id was made.
- **The allowlist switch.** Whether a server enforces its allowlist (`use_allowlist`) is not read
  or changed; only the entries are.
- **Operators.** They are not synchronised like the bans: they are read, never written, and only
  with `[operators] from-servers` (section 5.9, "The operators of the servers").
- **Kicks, the rest of the protocol.** Nothing else is read or sent.
- **Who banned and when.** The reason is the only note kept; the proxy keeps no author.

Tried against real servers on 2026-11-05 ([VALIDATION.md](VALIDATION.md)): three Horizon servers of
the build `02f05c1` on one machine, with scripted players, in both directions. Not tried with a real
client, over a network, with another Horizon build or with more than three servers. Try it
on one server first: ban a test name on the server, see `banned on <server>` in `banlist`, then ban
another at the console and read `banlist` on the server.

### 5.13 A lobby that sends players to other worlds (vanilla transfers)

A network of worlds that must **not** share a player's state (a lobby, a survival world, a hardcore world) cannot use
the proxy's `/server`: a switch carries the player's inventory, experience and game mode to the other server
([STATE-TRANSFER.md](STATE-TRANSFER.md)). Use vanilla's transfer instead: the lobby sends the client to another
address, the client connects again, and each server keeps its own player file.

With `accept-transfers = true`, the proxy takes these players. They log in **exactly** as a player who came on their own:
the same connection limits, bans, whitelist and account check (online mode), and the host they were sent to chooses
the server through `[[forced-host]]`. Off (the default), they are refused with vanilla's "transfers disabled".

```toml
# meridian.toml
listen = "0.0.0.0:25565"
accept-transfers = true
default-backend = "lobby"

[[backends]]
name = "lobby"
address = "127.0.0.1:25601"
[[backends]]
name = "survival"
address = "127.0.0.1:25602"
[[backends]]
name = "hardcore"
address = "127.0.0.1:25603"

[[forced-host]]
host = "play.<domain>"
backend = "lobby"
[[forced-host]]
host = "survival.<domain>"
backend = "survival"
[[forced-host]]
host = "hardcore.<domain>"
backend = "hardcore"
```

The three names point to the proxy's address (DNS records `play`, `survival` and `hardcore`). In the lobby, a command,
a sign or a plugin of the server runs vanilla's transfer for one player:

```
/transfer survival.<domain> 25565 <player>
/transfer hardcore.<domain> 25565 <player>
```

The proxy passes the server's transfer packet to the client untouched. The client then connects to `survival.<domain>`:
that is the proxy again, which logs the player in and sends them to `survival`. A host no `[[forced-host]]` names
leads to `default-backend`.

**The risk.** With `accept-transfers` on, **any** server on the internet can send a player to the proxy. That gives
the player nothing: the proxy checks them as any login, and their account is checked in online mode. Leave it off
if the network does not use transfers.

**Cookies.** A server can leave cookies on the client (`store_cookie`), which the client gives back only when asked.
The proxy never asks for one during the login and uses none to decide anything. It refuses one offered unasked, as
vanilla does. Once the player is on a server, that server may ask for the cookies another server left; the proxy
passes the request and the answer as they are, so what a cookie means is the servers' business.

`meridian_incoming_transfers_total{outcome}` counts them: `refused` (the setting is off), `logged_in` (their login with
the proxy finished: the account checked, the name and the limits of a login passed), and `failed` (taken, but that login did
not finish). The proxy's own checks come after the login, so a `logged_in` player can still be turned away by a ban, the
whitelist or a full proxy, as any player is; those refusals are counted with every login's (`meridian_login_failures_total{reason}`).

### 5.14 Compression: processor time against bandwidth

The proxy compresses every packet of `compression-threshold` bytes or more that it sends a player. On a busy network
this is the largest part of the proxy's processor time: about 41 % of it in a test with 100 players on one server,
almost all of it spent on large packets (the chunks of the world). `compression-level` chooses how hard the proxy
compresses them, from `1` to `9`; `6`, the default, is the level vanilla and Velocity use.

A lower level compresses faster and sends more bytes. In a measure of the compression alone, on packets captured from
test players (bots exploring a world, so mostly chunks), against level 6:

| Level | Processor time of the compression | Bytes sent |
| --- | --- | --- |
| 4 | about 69 % | about 3 % more |
| 3 | about 61 % | about 4 % more |
| 2 | about 51 % | about 10 % more |
| 1 | about 26 % | about 38 % more |

These figures are not a measure of a running proxy: how much of the proxy's processor time a level saves on a network
depends on how many chunks its players load, and real players who move less than those bots load fewer. The measure
below is of a running proxy.

**Measured on a running proxy: level 4 against level 6.** On 9 November 2026: 100 players (bots that explore a world)
on one real Horizon server, through the proxy, all on one machine (Windows 11, 32 logical processors: the server on 8
of them, the proxy on 4, the bots on 8 others), compression threshold 256 on both sides. Two pairs of runs, in the order
6, 4, 6, 4, each with 5 minutes of warm-up and then 10 minutes measured, with the same proxy binary at both levels:
only the setting changed.

| | Level 6 | Level 4 |
| --- | --- | --- |
| The proxy's processor time, two runs (cores, estimated from the process's cycles that Windows counts) | 0.440 and 0.438 | 0.390 and 0.384 |
| Mean | 0.439 | 0.387: 0.052 cores less, 11.8 % of the proxy |
| The bytes the players received, two runs (MB/s) | 8.81 and 8.74 | 8.92 and 8.91: 1.7 % and 1.5 % more than the mean of the two level-6 runs |

The rules were set before the test, and all held: both level-4 runs below both level-6 runs; the difference at least
twice the gap between the two runs of one level (0.006 cores at most: the difference is about nine times that); at least
3 % of the proxy; and the bytes at most 5 % more. Every run was valid: the server at 20 ticks a second on average (19.27
at the lowest sample), its tick time at the 95th percentile between 31.9 and 32.6 ms (under the 50 ms limit), no player
lost, and the same work from the server at both levels (its chunks within 2 %). Other programs of the machine used 0.66
to 0.73 cores during every run, the same at both levels within 0.07 cores: a reserve on the absolute figures, not on the
comparison. The proxy used about 3 MB more memory at level 4.

What this does not say: how real players behave (the bots load chunks all the time, so the compression is a large share
of the proxy's time; players who move less load fewer chunks, and a level saves less); another machine, more servers
or more players (one machine, one server, 100 players); level 3 (not measured on a running proxy); the packets towards
the servers (they keep level 6). This measure does not change the default, `6`.

**What it does not change.** The threshold, the packets towards the servers, the proxy's own messages (they keep level
6), and the players' side: zlib's format is the same at every level, and every client reads every level.

**When to change it.** Keep `6` unless the proxy's processor is the limit and the line has room for more bytes; then try
`4`, then `3`. Level `1` costs far more bandwidth for the little processor time it saves over `3`.

**How to measure it on a network.** Compare the same hour of play, with the same number of players, at least 10 minutes at
each level:

- the processor time of the proxy's process (`pidstat -u -p <pid> 60` or `top -p <pid>` on Linux; on Windows, the CPU
  column of the process in Resource Monitor);
- the bytes the machine sends (`vnstat -l -i <interface>` or `ip -s link show <interface>` read twice, a minute apart;
  on Windows, the Network tab of Resource Monitor, or `nethogs` for the process alone on Linux).

A reload applies the level to the players who connect after it; the players already connected keep theirs until they
reconnect. Restart the proxy, or wait for the players to come back, before measuring the new level.

## 6. Restarting a server without losing its players

A server that stops takes its players with it. There are four ways the proxy learns of it
([RESTART.md](RESTART.md) has the design):

| How | When | Needs |
| --- | --- | --- |
| The operator types `drain <server> --move`, wait, then stops the server | Before the stop. Planned | A console, servers that take transfers |
| The panel does it: `[drain-before-stop]` in the bench file, with `move-players = true` | Before the stop. Planned | The panel starts and stops the server |
| The server announces its own stop (`server/stopping` of its management protocol), heard by its sidecar | At the start of the stop. A Horizon server whose `[transfer] stop_wait_seconds` is above 0 (10 by default) waits for the players to be moved out, so there is time to move some of them (about 2 s each); with a wait of 0 it drops them about 78 ms later (measured on the build `02f05c1`): enough to drain it, not to move anyone. Planned or not, as long as the server says it | `--management-properties` on the sidecar, `management-server-enabled=true` on the server, `sidecar-notices = true` on the backend, a role and `[balancing] enabled` |
| The sidecar of the server does not answer for `silent-rounds` readings in a row | After the server is down | `[balancing] enabled` and a sidecar |

In all four cases the server is marked draining at once, so no new player is sent to it, and
it is offered again when it is back. The silence detection only drains: it does not move
anybody unless the evacuation is on.

```toml
[restart]
evacuate = false         # true: send the players of a server that is going down elsewhere
silent-rounds = 3        # readings in a row without an answer before it counts as going down
grace-seconds = 60       # a server told to stop may go on answering this long before it counts as back
```

`evacuate` is off by default. A Horizon `stop` with players placed now waits `[transfer] stop_wait_seconds` (10 s by default, 0 for none; Horizon's pull request #11713, merged on 8 November 2026 at 01:18 UTC) so that a proxy can move them out first: about 2 s a player, so 10 s is room for about four, and a fuller server needs a longer wait (Horizon's `docs/server.md`). Run 7 of [REAL-NETWORK-RUN.md](REAL-NETWORK-RUN.md) saw `evacuate` do it, with the proxy from before #146; it was not run again with the fixed proxy. With no wait, or at a second `stop`, the announcement comes about 78 ms before the server drops its players (measured on the build `02f05c1`, [VALIDATION.md](VALIDATION.md), B1), too soon for a move, and the fallback of section 5.11 catches them instead. It also helps when the server is announced early, as it is by the panel's `[drain-before-stop]`; to move players before a stop you type, send them first with `drain <server> --move`. When it is on, the players of the server go one at a time (half a
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
- [ ] Run as a service (section 5.7): a dedicated user, a configuration only that user can read,
      `systemctl reload` and `stop` tried once, and the log read in the journal.
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
- [ ] If you give players ranks: `permissions.toml` is in a folder the proxy can write and is kept
      with the configuration, `perm check` says what you mean for a player of each rank, and in
      offline mode nothing sensitive is given to a name (section 5.9).
- [ ] If you use the bans or the whitelist: `access.toml` is in a folder the proxy can write, is
      kept with the configuration, and a test ban and `pardon` of a test name work (section 5.8).
- [ ] If you turn `[access.sync]` on: each sidecar has `--management-properties` and its log says
      it is connected to the server's management protocol, a test ban on a server reaches
      `banlist`, and a test ban at the console reaches the server's list (section 5.12).
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
| `meridian_sessions_alive` | Sessions still in memory, over or not. It follows `meridian_players`; staying above it for long is a leak |
| `meridian_backend_players{backend}` | Players on each server |
| `meridian_backend_soft_cap{backend}`, `meridian_backend_hard_cap{backend}` | The caps of each server that has a role |
| `meridian_backend_state{backend,state}` | 1 for the state the balancer sees: `open`, `closed`, `draining` (drained, restarting, or not accepting transfers) or `not_offered` (no recent report) |
| `meridian_connections_total`, `meridian_connections_throttled_total` | Connections accepted, and the ones closed unread for opening too many in a minute |
| `meridian_incoming_transfers_total{outcome}` | Players other servers sent here with a vanilla transfer: `refused` (`accept-transfers` off), `logged_in` (their login finished; a ban, the whitelist or a full proxy can still turn them away), `failed` (section 5.13) |
| `meridian_logins_total` | Players who ended on a server |
| `meridian_login_failures_total{reason}` | Logins that did not: `protocol` (the handshake or login broke the protocol, or the connection failed), `duplicate`, `full`, `no_route`, `unreachable`, `banned` (including a connection closed for its address), `not_whitelisted`, `extension` (an extension refused the login, or refused the player at the choice of server, or could not be asked and its on-error policy refuses; [EXTENSIONS.md](EXTENSIONS.md) sections 4.2 and 4.4). A refusal by the authentication, and a server list ping, are not counted here |
| `meridian_transfers_total` | Seamless switches of a player to another server that succeeded, the player staying connected: `/server` and `/hub` in game, `/send` and the console's `switch` and `send`, an extension's `send` order, an evacuation. A player another server sent here with a vanilla transfer is counted in `meridian_incoming_transfers_total`, and a player whose server was lost, moved or not, in `meridian_fallbacks_total` |
| `meridian_transfer_failures_total{step}` | Transfers that failed, by step: `check`, `prepare`, `freeze`, `hand_over`, `commit` |
| `meridian_transfer_duration_seconds{phase}` | A histogram (buckets from 0.05 s to 10 s) of the phases of a successful transfer: `prepare`, `freeze`, `hand_over`, `commit`, and `total` (which includes reaching the sidecars) |
| `meridian_transfer_alerts_total` | The `ALERT` of the log: a source refused to let the player go after the target placed them. Alert on any increase (section 10) |
| `meridian_evacuations_total`, `meridian_evacuated_players_total{result}` | Evacuations of a server that had players, and the players moved or left where they were |
| `meridian_fallbacks_total{cause,result}` | Players whose server was lost in the game: `cause` is `kick` or `crash`, `result` is `moved` or `failed` (section 5.11) |
| `meridian_restarts_detected_total{cause}` | Servers found to be going down: `announced`, `panel`, `unreachable` |
| `meridian_status_rounds_total`, `meridian_status_round_duration_seconds` | Rounds of questions to the sidecars (with `[balancing] enabled`), and how long the last took |
| `meridian_relay_packets_total{direction}`, `meridian_relay_writes_total{direction}` | The packets the relays sent the players, counted one by one as they were framed, and the writes they took; `direction="to_player"` is the only direction. The relay writes the packets that one read of a server brought in one write, at most 64 packets or 32 KiB of payload at a time (a join's world takes several), so the first over the second is how many packets a write carries (about one when a server writes each packet on its own, more when it writes a tick's at once). Only the relay's own writes are counted: not a transfer's delta, nor the proxy's messages to a player, nor the answers to commands. Both count every session of the proxy since it started |

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
| `INFO <player> is already on <server> (moved by the fallback)` | The server stopped before the evacuation reached this player, and the fallback had moved them: they are counted apart (`<k> already moved by the fallback`), not as staying |
| `WARN switch-freeze-limit-ms ...` at start | The proxy's limit is not below Horizon's `max_freeze_seconds` |
| `<n> connections are still open; they close when the proxy exits` | A stop did not finish in five seconds |
| `servers` showing a server that never gets players | Drained, not heard from, at its cap, or in a group for the other kind of player |

The panel (section 9) shows each process's state, processor and memory use.

### 8.2 Memory and file descriptors

A proxy that runs for weeks must give back what a player took when the player leaves. Two readings tell
whether it does:

- **`meridian_sessions_alive`** (section 8.1) counts the sessions made and not freed yet. When players leave it
  falls back to `meridian_players`, and with nobody connected it is 0. A figure that stays above
  `meridian_players` for minutes is a session that nothing frees.
- **The open file descriptors of the process.** On Linux, `ls /proc/$PID/fd | wc -l`, with the process id from
  `systemctl show -p MainPID --value meridian`. A proxy at rest holds a handful (7 in the tests), whatever it served
  before. Read it again after a busy spell: it must be back to what it was.

A leak of the kind that was found shows in both at once: the descriptors rise by 4 for each player who joined and
are never given back, `meridian_sessions_alive` stays above `meridian_players`, and the memory at rest rises with
them (about 6 MiB for each 150 players in that test). The proxy then refuses connections with "Too many open files"
when the descriptors reach the unit's `LimitNOFILE=65536` (section 5.7), long before the machine is out of memory.
Alert on `meridian_sessions_alive` staying above `meridian_players`, not on the memory.

How it was found, the figures before and after the fix (#119), and what the check did not cover are in
[MEMORY.md](MEMORY.md).

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
| Operating without a terminal | The panel types commands for a proxy it starts. On Unix, SIGTERM stops the proxy and SIGHUP reloads it (section 5.3) | A remote console or administration port. On Windows, a signal for `reload` |
| Logs | One line per event on standard error, in UTC | A log file, rotation, a structured format. Let the service manager keep them |
| Service files and packages | `meridian` for Linux and Windows x86_64 in release archives, with a checksum file and an attestation. A systemd unit and a Dockerfile for the proxy, in the repository (section 5.7) | A published image, a package, and a unit or an image for the sidecar or the panel. The unit and the Dockerfile were not run on a real host. The sidecar and the panel are built from the sources |
| Several proxies, or a proxy that restarts without dropping players | Nothing | A second proxy for the same servers: the counts and the caps are per proxy, and players on a proxy that dies are dropped |
| Placing players | Caps, a drain, balancing among lobbies and `/play` for games, evacuation of a restarting server | Moving a group from a lobby to a game, matchmaking, a decision on who starts a move |
| Waiting when full | `[queue]`: a bounded line, first come first served, for a full proxy or lobbies at their hard cap (section 5.10) | A longer wait with a message of position, a line per group or role, priority |
| Flood protection | `connections-per-minute` per address, `max-players`, `[auth] lookups-per-minute` for the hybrid mode | A firewall or a DDoS service in front of the port; the proxy is not one |
| Player management | `kick`, `ban`, `pardon`, `banlist` and `whitelist` (section 5.8), their sync with the servers' own lists (section 5.12, off by default), groups of permissions with `perm`, `/send`, `/glist` and `/alert` in game (section 5.9), `max-players`, `on-duplicate`, and each server's own whitelist and bans | In-game commands for the bans and the groups, an expiry on a ban or a rank, a player database in the proxy |
| Seamless moves between servers | `switch`, `send`, `/server`, `/hub`, `/lobby`, `drain --move` against servers with the transfer module | That module in a released Horizon, and a recorded run on real servers |
| Early warning of a restart | `drain --move`, the panel's `[drain-before-stop]`, and detection by the sidecar's silence. Horizon's `server/stopping` notice reaches the proxy (run on real servers, [VALIDATION.md](VALIDATION.md)) and, with Horizon's stop wait (`stop_wait_seconds`, 10 s by default, #11713, merged on 8 November 2026 at 01:18 UTC), the players are not dropped until they are moved or the wait ends; with a wait of 0 it was about 78 ms before they were dropped (build `02f05c1`) | The evacuation run again with the corrected proxy and the stop wait |
| Report of a server's state | The sidecar's `status` request, which is Horizon's `transfer-status`, and the proxy's reading of `online` and `max_players` (and `accepts_transfers`, from the error of a server with transfers off) | Not run against a real Horizon yet. Against one from before `transfer-status`, balancing counts the proxy's own sessions |

## 12. Known limits

- Few runs were made on a real network: 250 bots against a proxy on a laptop across a home
  network ([LOAD-LAN-250.md](LOAD-LAN-250.md)), one real client over the Internet to a proxy
  on a rented server, with 250 bots on the same machine ([LOAD-VPS-250.md](LOAD-VPS-250.md),
  [REAL-CLIENT-RUNS.md](REAL-CLIENT-RUNS.md)), and the moves and the stops of [REAL-NETWORK-RUN.md](REAL-NETWORK-RUN.md)
  (6 and 7 November: three real clients on one PC, real Horizon servers). Each is one run or one
  session with its limits; the load runs went to stand-in servers; the load figures in
  [BENCH.md](BENCH.md) are local. Plan your own test with your players' numbers before opening.
- The seamless switch needs a Horizon build with the transfer module, which was merged into
  Horizon's main on 2026-11-05 and is not in the Nightly of that day (section 0). It was run on
  real servers with real clients over a real network on 6 November 2026, in one session with the
  three clients on the same PC as the second server (section 6 of [REAL-NETWORK-RUN.md](REAL-NETWORK-RUN.md)
  lists what was not done); a public comment describes earlier moves with a real client on one
  machine, not repeated ([REAL-CLIENT-RUNS.md](REAL-CLIENT-RUNS.md), section 2).
- Movement through the proxy was run with real clients over a real network in one session
  ([REAL-NETWORK-RUN.md](REAL-NETWORK-RUN.md)), and before that with a real client on one machine (a public comment,
  [REAL-CLIENT-RUNS.md](REAL-CLIENT-RUNS.md), section 2). Many players and long sessions were
  not tried against a real server (the 250 bots of [LOAD-VPS-250.md](LOAD-VPS-250.md) went to
  stand-in servers). The online mode's account check was run once against the real account service, with a real
  Horizon client and a real Java client and one account ([REAL-CLIENT-RUNS.md](REAL-CLIENT-RUNS.md), section 5). The
  hybrid mode's name lookup, several accounts at once, long sessions, an account with no licence and the queue in online
  mode have not been tried.
- A server that stops without warning, or disconnects a player, moves them to another server
  (section 5.11) as a new arrival, with a loading screen, and what was unsaved on the lost server
  is lost. This ran with a real client over the Internet, once each for a killed server, a kick
  and a clean stop ([REAL-CLIENT-RUNS.md](REAL-CLIENT-RUNS.md), section 3). A planned restart moves its players without a
  loading screen, with the transfer module (section 6), and so did a stop that waits for the
  evacuation, which Horizon's pull request #11713 added (`[transfer] stop_wait_seconds`, merged on 8 November 2026 at 01:18 UTC; run 7 of [REAL-NETWORK-RUN.md](REAL-NETWORK-RUN.md)). A player still logging in is not moved.
- Horizon's `server/stopping` notice and the panel's announcement are the signals for an early
  warning. The first is fed only when the sidecar is given `--management-properties` and the backend
  has `sidecar-notices = true`; the sidecar reads the management protocol on its own machine and
  does not verify the self-signed certificate Horizon makes ([RESTART.md](RESTART.md), "How the
  announcement reaches the proxy"). It was run against real Horizon servers ([VALIDATION.md](VALIDATION.md)): the announcement reaches the proxy within a few milliseconds, but on that build (`02f05c1`) the server dropped its players about 78 ms after it, so it was a signal to drain, not time to move. Horizon's stop wait (#11713, merged on 8 November 2026 at 01:18 UTC) now gives time to move some of them (section 6).
  The second needs the panel to supervise the server.
- The server's own report of its state (`accepts_transfers`, `players`, `capacity`) is not
  defined by Horizon yet; balancing counts the proxy's own sessions.
- The sidecar has no TLS, no encryption, and the proxy has no way to check one other than a
  private network.
- Signed chat is not supported across the proxy: servers behind it must not enforce secure
  chat.
- The proxy shows its own server-list message, not the server's. `/server` is not completed by
  Tab.
- SIGTERM and SIGHUP are handled on Unix only (section 5.3); on Windows only Ctrl+C stops the
  proxy. The console is standard input (section 5.4).
- The permissions by name are as strong as the login, like the whitelist: in offline mode a name
  is not proof of who is playing, and a rank given to a name goes to whoever types it (section
  5.9). The proxy decides only the commands it answers itself; what a server's own commands allow
  is the server's.
- The bans and the whitelist by name are as strong as the login: in offline mode, and for the
  unverified players of the hybrid mode, a name is not proof of who is playing (section 5.8). A
  ban by address is closed without a reason.
- The sync of the bans and whitelist with the servers (section 5.12) ran against three real Horizon
  servers on one machine with scripted players ([VALIDATION.md](VALIDATION.md)); it was written
  and first tested against stand-ins that use the methods and shapes of Horizon's management
  protocol as written in its source. It does not carry ranges of addresses or bans
  that end, and a change that could not be sent to a server in six tries is not sent again.
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
  Velocity proxy". The transfer module: [STATE-TRANSFER.md](STATE-TRANSFER.md) and the
  pull requests named in section 0.
