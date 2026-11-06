# Parity with Velocity and BungeeCord

This page compares what Meridian does today with what proxies such as Velocity and
BungeeCord are known for, function by function, and ranks what to build next for a
network of Horizon servers. It is a read-only audit: no code changed with it.

**Where the list comes from.** The functions are those the two projects describe publicly
to their users (settings, commands, documented behaviour) and that a proxy of this kind is
seen to do, as in [DESIGN.md](DESIGN.md) section 14. No source code of either project was
read or copied, and none is quoted here. **Where the Meridian column comes from.** Every
"exists" and "partial" row was checked by searching the code at commit `7b8ab69` (main after
pull request #77), and cites `file:line`. A row that says "missing" means the search found
nothing, with the words searched in the notes at the end. Nothing here was run against real
servers.

## How to read the tables

- **Status:** *exists* (works and is tested in CI), *partial* (some of it exists; the row says
  what is left) or *missing*.
- **Difficulty:** *easy* (a day or two, one crate, no new design), *medium* (several days, a
  new module or a protocol detail to settle), *hard* (a design question, several crates, or
  a change on Horizon's side).
- **Value:** for a network of Horizon servers behind one proxy: *high*, *medium*, *low*.
- **Horizon:** what the function needs from Horizon. "None" means the proxy can do it alone.
  The transfer module is the set of Horizon pull requests (#8700, #8709, #8716, #8730,
  #8735, #8742), merged into Horizon's main on 2026-11-05 ([OPERATING.md](OPERATING.md) section 0).

## 1. Routing and resilience

| Function | Status | Where in Meridian, or what is missing | Difficulty | Value | Horizon |
| --- | --- | --- | --- | --- | --- |
| Fallback when a backend refuses the player at login | exists | Each candidate is tried in turn; a login refusal from the backend (`Disconnect` in the login phase) is an error and moves on to the next: `meridian-net/src/backend.rs:99`, `meridian-proxy/src/session.rs:581`. A server that failed is tried last for `backend-retry-seconds`: `health.rs:42` | n/a | high | None |
| Fallback list (`try`) | exists | `route.rs:96-99` (the list follows the default server or the group), `meridian-config/src/lib.rs:384` | n/a | high | None |
| Fallback when a backend kicks or crashes a player already in the game | exists, run with a real client over the Internet, once each for a killed server, a kick and a clean stop ([REAL-CLIENT-RUNS.md](REAL-CLIENT-RUNS.md), section 3) (added after this audit) | `meridian-handoff/src/session.rs` (`Session::run_with`, `lost`, `begin_moving`, `activate`) keeps the player and sends the client back to the configuration phase; `meridian-proxy/src/fallback.rs` chooses the server (`route::candidates`, `placing::login`, the health of the backends), called from `session.rs` after `relay.run_with`. `[fallback]`: `meridian-config/src/fallback.rs`. Tests: `session/tests/fallback.rs`, `meridian-handoff/src/tests/fallback.rs`. A cold move: the lost server's state is gone | n/a | high | None (a new login and configuration; the transfer module is not used) |
| Planned restart without dropping players | exists | `drain <server> --move`, `[restart] evacuate`: `evacuate.rs:51`, `evacuate.rs:98`, detector `restart.rs:103`, announcement from the server `notices.rs:257` and `meridian-sidecar/src/management.rs:183-194` | n/a | high | The transfer module for the move; the server's own `server/stopping` announcement for the early warning (not tried in a recorded run) |
| Forced hosts | exists | `meridian-config/src/lib.rs:813` (`forced_backend`), used first by `route.rs:65`; `[[forced-host]]` at `lib.rs:387` | n/a | medium | None |
| Groups of servers by kind of player | exists | `[[group]]`, `[[route]]`: `route.rs:59-111`, `lib.rs:718`. These groups pick servers for verified and unverified players; they are not groups of players | n/a | medium | None |
| MOTD per forced host | exists | `motd` on `[[forced-host]]`: `meridian-config/src/lib.rs:67`, read at `lib.rs:560`; `Config::motd_for` (`lib.rs:828`) finds the host by the rules of `forced_backend` (`lib.rs:819`, one lookup for both). The handshake's host reaches the status handler: `client.rs:192-196` (`virtual_host`, as for a login), `client.rs:599` (`serve_status`), `client.rs:648`. Without a `motd` on the host the global one is shown. Tests: `client.rs:813`, `favicon_tests.rs`. A MOTD per *route* (per kind of player) does not exist: a status request has no account | n/a | medium | None |
| Hub command and default server | exists | `/hub`, `/lobby`, `hub-backend`: `commands.rs:112`, `meridian-config/src/commands.rs:99` | n/a | high | None |
| Load balancing among lobbies | exists | `placing.rs:44` (`login`), `balance.rs:413`; roles, caps, drain ([LOAD-BALANCING.md](LOAD-BALANCING.md)). Beyond what the two proxies do natively | n/a | high | The server's own count: balancing counts the proxy's sessions today |

## 2. Server list

| Function | Status | Where in Meridian, or what is missing | Difficulty | Value | Horizon |
| --- | --- | --- | --- | --- | --- |
| Own message of the day, online count, shown maximum, sample of names | exists | `client.rs:599-657` (`serve_status`, `status_json`), `max_shown` at `client.rs:660`, `status-sample` at `lib.rs:186` | n/a | high | None |
| Total players of the network | partial | The count is the players this proxy relays: `client.rs:612` (`players_online`). With one proxy it is the total. It is not summed over the servers, nor over several proxies | easy (one proxy); hard (several, see section 6) | medium | None |
| Favicon | exists | `favicon = "file.png"`: `meridian-config/src/lib.rs:146`, `favicon.rs:41` (read at the start and at each `reload`) and `favicon.rs:57` (a 64x64 PNG of at most 16 KiB, read from its header, ending with `IEND`; an invalid file stops the start or refuses the reload). The status reply carries it as `data:image/png;base64,...`: `client.rs:653`. One picture for the whole proxy, none per forced host. Test: `client.rs:842` | n/a | medium | None |
| Ping passthrough (a server's own reply) | **missing** | The proxy answers alone, by design, so that a ping opens no second connection: `client.rs:625-632`. [README.md](../README.md) lists it as planned. The sidecar's `status` request exists (`status.rs:207` polls it) and could feed a cached answer | medium | medium | The server's own status, or a Horizon command that reports it |
| Version shown | partial | Always Horizon's: `client.rs:643-646`. Fine with one game version | easy to change | low | None |
| Query protocol | missing, out of scope | Stated out of scope in [DESIGN.md](DESIGN.md) section 14 | medium | low | None |

## 3. Commands and messages

| Function | Status | Where in Meridian, or what is missing | Difficulty | Value | Horizon |
| --- | --- | --- | --- | --- | --- |
| `/server`, `/hub`, `/lobby`, `/play` in game | exists | `commands.rs:102`, `:112`, `:128`, each behind its permission node ([OPERATING.md](OPERATING.md), section 5.9); the words are fixed in `meridian-config/src/commands.rs:9-49`. `/server` is not completed by Tab (README) | n/a | high | The transfer module for a move without a loading screen |
| `/send` (move a player or everyone) | exists | In game: `commands.rs:143` (the command and its permission), `staff.rs:177` (`run`), which moves players in a thread of its own through `staff::move_all` (`staff.rs:18`), the code the console's `send` now shares (`console.rs:416`). `/send <player\|all\|server:name> <server>`; off unless listed in `[commands] enabled`; needs `meridian.send` ([OPERATING.md](OPERATING.md), section 5.9). The move itself needs the transfer module | n/a | high | The transfer module |
| `/glist` (who is online, per server) | exists | In game: `commands.rs:155`, `staff.rs:82` (`glist`, one line for each server). `/glist [server]`; needs `meridian.glist`. The console keeps `list` and `servers` (`console.rs:229`, `console.rs:264`) | n/a | medium | None |
| `/alert` (message to all, or to a server) | exists | In game: `commands.rs:165`, `staff.rs:128` (`alert`), which writes a system chat line to each session with `SessionHandle::tell` (`meridian-handoff/src/session.rs:282`). `/alert [server:name] <message>`; needs `meridian.alert`. The console has `alert` too (`console.rs:456`). Missing: a title or action-bar form | n/a | medium | None |
| Global chat | **missing** | Chat passes through to the player's own server and stays there; the relay "interprets nothing" (`relay.rs:1-12`). A merged chat needs the proxy to read the chat packets, which works only while secure chat is off (README, known limits) | medium | medium | None, if servers keep secure chat off |
| Private messages (`/msg`) across servers | **missing** | Same as above: no search for `msg` or `tell` in the proxy | medium | medium | None |
| Tab list merged across servers | **missing** | The player sees the list of the server they are on. `meridian-ghost/src/tab.rs:1` models the list so that a switch can replace it, but nothing merges lists. [DESIGN.md](DESIGN.md) section 14 calls it a plugin matter | hard | low | None |
| Resource pack forwarding and control | partial | Relayed unchanged in a normal login. A switch is refused when the target pushes a pack in its configuration phase: `meridian-handoff/src/configure.rs:154`. No proxy-wide pack setting. Out of scope for sending packs: [DESIGN.md](DESIGN.md) section 14 | medium | medium (high if the servers use packs) | Whether Horizon servers push packs, and how a client keeps or replaces one |
| Plugin messaging (channels) | partial | Custom payloads pass through the relay untouched; nothing in `meridian-proxy` or `meridian-net` reads a channel (no `CustomPayload`, `brand` or channel name outside the bench). No messages between servers: [DESIGN.md](DESIGN.md) section 13.4 says Horizon's `horizon:mod_net` links a server to its own players only | medium | medium | A hook in Horizon for a mod to use a channel (DESIGN.md section 16) |
| Custom commands and an extension API | **missing** | `[commands]` turns six fixed commands on or off (`meridian-config/src/commands.rs:9-23`). No trait, no events: [DESIGN.md](DESIGN.md) section 13.5 (J7), README "Extensions: planned" | hard | high | None for the proxy; hooks in Horizon for data between servers |

## 4. Players, rights and protection

| Function | Status | Where in Meridian, or what is missing | Difficulty | Value | Horizon |
| --- | --- | --- | --- | --- | --- |
| Kick | exists | Console `kick`: `console.rs:474`, `session.rs:379`. Players are told why | n/a | high | None |
| Duplicate login (refuse or replace) | exists | `on-duplicate`: `meridian-config/src/lib.rs:235`, `session.rs:263` | n/a | medium | None |
| Player limit | exists | `max-players`: `route.rs:144` (`admit`), `session.rs:221` | n/a | high | None |
| Permissions and ranks | exists | Groups with inheritance and nodes, and the players in them (by name or unique id), in `permissions.toml`: `meridian-config/src/permissions.rs` (`allows` at `:202`), `meridian-proxy/src/permissions.rs`, console `perm` (`permissions/commands.rs:31`). Read at the start, at `reload` and on SIGHUP. The commands in game check it (`commands.rs:92`): `/server`, `/hub`, `/play` stay open until denied, the staff commands need a node ([OPERATING.md](OPERATING.md), section 5.9). Missing: commands in game to manage the groups, a rank that expires, a group for a kind of login, a permission for a backend's own commands. In offline mode a name proves nothing | n/a | high | None for the proxy's own commands |
| Bans at the proxy (name, unique id, address) | exists | Console `ban`, `pardon`, `banlist`; names, unique ids, addresses and ranges, each with a reason: `meridian-config/src/access.rs`, `meridian-proxy/src/access.rs`, `access/commands.rs`. Applied before any server is asked: address in `serve.rs` (before a byte is read), name and unique id in `session.rs` (`serve`). Kept in `access.toml`, read at the start and at `reload` ([OPERATING.md](OPERATING.md), section 5.8). Missing: in-game commands for the bans and an expiry. The sync with the servers' lists is the row below | n/a | high | None |
| Proxy whitelist | exists | Console `whitelist add\|remove\|list\|on\|off`, by name or unique id, off by default, in the same `access.toml`. Applies to those who join, not to players already connected. In offline mode a name is not proof, so a whitelist by name is no barrier there (OPERATING.md, section 5.8) | n/a | high | None |
| Mute at the proxy | **missing** | Needs the proxy to read chat (see global chat) | medium | low | None |
| Sync of bans and whitelist with the servers' management protocol | exists | `[access.sync]`, two directions, both off by default ([OPERATING.md](OPERATING.md), section 5.12). The sidecar's connection to the server's management protocol, which only listened, now also makes calls and keeps the list notices apart from the stop notices: `meridian-sidecar/src/lists.rs` (`Lists`, `call` at `:279`, `answer` at `:331`), `management.rs:394` (`listen`), and two signed requests, `access-notices` and `manage`, served at `server.rs:179` and `server.rs:212`; only the three lists' read, add and remove go through. `from-servers`: `meridian-proxy/src/access/sync/hear.rs` (`round` at `:164`, `apply` at `:306`), the notifications and lists read as Horizon writes them in `events.rs:58` and `:119`; a ban is noted `banned on <server>`, a pardon lifts only that mark, the lists are read again when the sidecar connected again, and nothing local is ever removed. `to-servers`: `push.rs` (`Change::call` at `:77`, `Outbox` at `:190`, `start` at `:306`), called from the console's `ban`, `pardon` and `whitelist` (`access/commands.rs:47`); one lane per backend, in order, six tries from 2 s to 2 min, never on the console's or a login's path. No loop: a change that comes from a server is never sent on, the proxy's own ban comes back with the source `Meridian` and is ignored. Not synced: ranges of addresses (Horizon bans single addresses), bans with an expiry, the allowlist switch, who banned. Tested against stand-ins that use Horizon's methods and shapes as read on its main branch (`horizon-server/src/management.rs`, `management_methods.rs`); run on 2026-11-05 against three real Horizon servers (build `02f05c1`) with scripted players, over the loopback ([VALIDATION.md](VALIDATION.md), section 7) | n/a | medium | A run with a real client and over a network |
| Connection limit per address | exists | `connections-per-minute`: `throttle.rs:24`, used before any packet is read: `serve.rs:90`; counted on the player's address behind a load balancer (`serve.rs:76`) | n/a | high | None |
| Anti-bot beyond the rate limit | partial | Login and read timeouts (`client.rs:30`, `client.rs:180`, `client.rs:34`), `[auth] lookups-per-minute`, the hybrid mode's name checks (`login_policy.rs:208`). Missing: a cap of simultaneous connections per address, a cap of new connections per second overall, name patterns, a quarantine for a new address | medium | medium | None |
| Real address behind a load balancer | exists | PROXY protocol v1 and v2: `meridian-net/src/proxy_protocol.rs`, `serve.rs:76` | n/a | high | None |
| Online, offline and hybrid login | exists | `login_policy.rs:208`; forwarding to the servers `meridian-net/src/backend/forwarding.rs:24` | n/a | high | None |

## 5. Operation

| Function | Status | Where in Meridian, or what is missing | Difficulty | Value | Horizon |
| --- | --- | --- | --- | --- | --- |
| Reload | partial | The console command `reload`: `console.rs:501`. `listen` needs a restart | n/a | high | None |
| Reload by signal (SIGHUP) | exists on Unix (added after this audit) | `meridian-proxy/src/signals.rs`, through `console::reload_file`, the code of `reload`; a test sends the real signal (`meridian-proxy/tests/signals.rs`). None on Windows | n/a | high | None |
| Clean stop on SIGTERM | exists on Unix (added after this audit) | `signals.rs` stops the proxy as Ctrl+C does (`ctrlc` stays for SIGINT and Windows; `signal-hook` for the two signals, since `ctrlc`'s `termination` feature cannot tell them apart). The clean stop: `serve.rs:21`, `session.rs:389` | n/a | high | None |
| Log file | **missing** | Standard error only: `log.rs:52`. [OPERATING.md](OPERATING.md) section 11 says to let the service manager keep them | easy | low | None |
| TLS | partial, by design | None on the player's leg (the game protocol has none; the login is encrypted in online mode, `client.rs:352`). None on the proxy-to-server leg (plain after the login). The sidecar reaches the server's management protocol over TLS without checking the certificate: `meridian-sidecar/src/tls.rs`. The metrics page is plain HTTP on loopback | hard (proxy-to-server) | medium | Horizon accepting a protected backend leg |
| Service file and container image | exists, not run on a host (added after this audit) | `deploy/meridian.service` (passes `systemd-analyze verify`) and `Dockerfile` with `.dockerignore` ([OPERATING.md](OPERATING.md) section 5.7). The release workflow still builds binaries only (`.github/workflows/release.yml:41`): no image is published | n/a | high | None |
| Metrics and health check | exists | `metrics.rs:632`, `/metrics` and `/healthz` at `metrics.rs:610-607` | n/a | high | None |
| Remote console | **missing** | The console is standard input ([OPERATING.md](OPERATING.md) section 5.4). The panel types commands for a proxy it started | medium | medium | None |
| Updating the proxy without a cut | **missing** | The listener sets `SO_REUSEADDR` only (`serve.rs:43`); a second proxy cannot share the port, and the players of a stopping proxy are dropped. The seamless switch is not used for this | hard | medium | The transfer module, if players were to be handed to another proxy (not designed) |
| Starting and stopping servers automatically | partial | `meridian-panel` starts, stops and restarts the servers of a local test bench: `supervisor.rs:215`, `:301`, `:355`. It serves `127.0.0.1` only and is not part of the proxy. Nothing starts a server because players need one | hard (production) | medium | A way to start a Horizon server from outside |

## 6. Scale and placement

| Function | Status | Where in Meridian, or what is missing | Difficulty | Value | Horizon |
| --- | --- | --- | --- | --- | --- |
| Several proxies with shared state | **missing** | The registry, the counts, the caps, the throttle and the duplicate check are in memory in one process: `session.rs:91`, `state.rs:19`, `throttle.rs`. [OPERATING.md](OPERATING.md) section 11: counts and caps are per proxy | hard | high (when one proxy is not enough) | None |
| Queue when the network is full | partial | `[queue]` (off by default): a bounded wait, first come first served, for a full proxy (`max-players`) or lobbies at their hard cap. The line: `meridian-proxy/src/queue.rs:55` (`LoginQueue`), `queue.rs:136` (`take`, which makes a newcomer go behind the line), `queue.rs:170` (`await_room`, the lobby case), woken by `session.rs:193` (a place given back), `state.rs:75` (a reload) and `session.rs:393` (stop); wired in `session.rs:502-527` and `session.rs:619`. Settings: `meridian-config/src/queue.rs`; the console's `queue`; metrics `meridian_queue_*`. What is left: the wait is at most 20 s, because the proxy holds the login in silence and the client gives up after 30 s; no message of position, no keep-alive, no line per group or role (section 5.10 of [OPERATING.md](OPERATING.md)) | hard (a longer wait) | medium | None |
| Groups of players (parties) | **missing** | No party code. The `[[group]]` of the routing is a group of servers | medium (a party that moves together uses `send`) | medium | The transfer module for a group move without a loading screen |
| Matchmaking | partial | `/play` finds a server of role `game` with room: `commands.rs:128`, `placing.rs:212`. No pairing of players, no ready check, no session that fills then closes | hard | medium | The server's own state (`accepts_transfers`, `players`) from a Horizon command that does not exist yet |
| Moving a player without a loading screen | partial | The seamless switch is built and tested on fake servers: `meridian-handoff`, `switching.rs`. No recorded run on real servers; a public comment describes moves with a real client on one machine, not repeated ([REAL-CLIENT-RUNS.md](REAL-CLIENT-RUNS.md), section 2) | n/a | high | The transfer module; a real run ([TESTING-FORK.md](TESTING-FORK.md)) |
| Carrying the position at a transfer (#8735) | partial | `switch-carry-position`, off by default: `meridian-config/src/lib.rs:198`, `meridian-handoff/src/switch.rs:169` and `:200`. It needs servers that take the place in the import request, "Horizon from fork commit 46bfe5de0" ([CONFIG.md](CONFIG.md)). Horizon's `docs/server.md` ("Where the player arrives") now describes the request fields `position`, `rotation`, `on_ground` and `velocity`, merged into its main on 2026-11-05; not run against a real server | easy (the proxy side is done) | high | The pull request that takes the place in the import; both servers in the same dimension |

## 7. Protocol

| Function | Status | Where in Meridian, or what is missing | Difficulty | Value | Horizon |
| --- | --- | --- | --- | --- | --- |
| Compression | exists | A threshold for the player's side (`compression-threshold`, `client.rs:509-513`); the server's side follows what the server asks: `backend.rs:67`. Each side may differ (`relay.rs:1-12`) | n/a | high | None |
| Encryption | partial, by design | The player's side in online mode (`client.rs:352`). The server's side is plain TCP after a signed login ([OPERATING.md](OPERATING.md) section 11) | hard (server's side) | medium | Horizon accepting one |
| Versions of the game | exists, one only | One protocol version, Horizon's; an older or newer client is told so as vanilla does: `client.rs:232-241`. A proxy that speaks several versions (translating packets) is out of scope: [DESIGN.md](DESIGN.md) section 14 | hard | low | None |
| Player information forwarding | exists | Horizon's Velocity-style exchange: `meridian-net/src/backend/forwarding.rs:24` | n/a | high | None |

## Top 10, in the order to build them

The order weighs value for a Horizon network against the cost, and puts a function after the
one it depends on. Items 1 and 2 are what a service manager needs before anything else runs
in production.

| # | Function | Difficulty | Why it ranks here |
| --- | --- | --- | --- |
| 1 | SIGTERM stop, then SIGHUP reload (section 5). **Done on Unix after this audit** | easy, then medium | `systemctl stop` and `docker stop` send SIGTERM; today the proxy may be killed with sessions open. SIGTERM is a feature flag of a crate already used; SIGHUP needs the two signals told apart |
| 2 | Service unit and container image, in the release (section 5). **Written after this audit; publishing the image is not done** | easy | The first thing an operator asks for, and it needs no change to the proxy. Nothing else on the list has so small a cost for its value |
| 3 | Favicon and a MOTD per forced host (section 2, section 1): done, both exist | easy | Seen by every player, before they join. The status handler needs the handshake's host; the favicon is one field of the reply. Ping passthrough can come later, from the sidecar's cached state |
| 4 | Bans and whitelist at the proxy (section 4), **done** | medium | Applied before any server is asked, in one place for the whole network. A file read at the start and at `reload`, with `ban`, `pardon` and `whitelist` on the console. The sync with Horizon's management protocol is `[access.sync]`, off by default, and ran on 2026-11-05 against three real servers with scripted players (VALIDATION.md) |
| 5 | A permissions file (section 4), **done** | medium | The base of everything an operator types in game. Without it every in-game command is for everyone or for nobody |
| 6 | `/send`, `/glist`, `/alert` in game (section 3), **done** | easy, after 5 | The console already has the logic (`console.rs:229`, `staff.rs:18`) and the relay can write a chat line to a player (`relay.rs:306`). It became a command through the permissions |
| 7 | Fallback when a backend kicks or crashes a player in game (section 1). **Done, as a cold move; run with a real client over the Internet, once each for three cases** | hard | The largest hole in resilience: a crash today drops every player on that server. Start with a cold move (a new login on the fallback, the loading screen), which needs nothing from Horizon; the seamless version waits for the transfer module |
| 8 | Queue when the network is full (section 6): first level done (a bounded wait), the longer wait is left | medium | The two refusal points were `route.rs:144` and the hard-cap refusal in `session.rs`; both now go through `queue.rs`. The wait is held in silence, so it is at most 20 s; a longer one needs the player kept in the configuration phase with keep-alives (see the report of this change), and is invisible to Horizon |
| 9 | An extension API as a Rust trait with events (section 3) | hard | J7 of the design. It is what turns the missing items (chat, party, matchmaking) from proxy code into code an operator can write, without the proxy growing |
| 10 | Global chat and private messages (section 3) | medium | Wanted on every network of more than one lobby. Last because it needs the proxy to read chat packets, which works only while secure chat is off, and because item 9 would let it be an extension |

Not in the top 10, and the reason:

- **Several proxies with shared state:** the answer to a proxy that dies, but a design of its own
  (a store the counts and caps live in, or an election). Worth planning once one proxy is
  measured at its limit; [BENCH.md](BENCH.md) has the only figures, local ones.
- **Resource pack handling at a switch:** a switch fails when the target pushes a pack
  (`configure.rs:154`). It becomes the first item if Horizon servers use packs.
- **A merged tab list, mute, query protocol, several game versions:** low value for the cost,
  or out of scope in the design.
- **Carrying the position:** the proxy side is done; it waits for the Horizon pull request and
  for the first real run.

## What was searched

Each "missing" row was checked by searching `crates/` (Rust files and manifests) for these words,
case-insensitively: `favicon`; `glist`, `alert`, `broadcast`, `"say"`; `msg`, `tell`, `private message`,
`PlayerChat`; `permission`, `ban`, `bans`, `whitelist`, `allowlist`, `blacklist`; `queue`, `party`,
`matchmak`, `redis`, `cluster`; `SIGTERM`, `SIGHUP`, `signal`; `Dockerfile`, `*.service`;
`CustomPayload`, `plugin message`, `brand`, `ResourcePack`; `trait` ending in `Hook`, `Extension`
or `Plugin`. The matches that were not the function (a mutex, a `Disconnect` in a test, the bench's
echo) are left out of the tables. A function that has another name in the code may have been
missed: a report of one is a correction to this page.

What this audit did not do: run the proxy, read Velocity's or BungeeCord's code, or check which
methods of the management protocol Horizon serves.
