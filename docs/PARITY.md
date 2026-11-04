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
  #8735, #8742), open on 2026-11-04 ([OPERATING.md](OPERATING.md) section 0).

## 1. Routing and resilience

| Function | Status | Where in Meridian, or what is missing | Difficulty | Value | Horizon |
| --- | --- | --- | --- | --- | --- |
| Fallback when a backend refuses the player at login | exists | Each candidate is tried in turn; a login refusal from the backend (`Disconnect` in the login phase) is an error and moves on to the next: `meridian-net/src/backend.rs:99`, `meridian-proxy/src/session.rs:480`. A server that failed is tried last for `backend-retry-seconds`: `health.rs:42` | n/a | high | None |
| Fallback list (`try`) | exists | `route.rs:96-99` (the list follows the default server or the group), `meridian-config/src/lib.rs:357` | n/a | high | None |
| Fallback when a backend kicks or crashes a player already in the game | **missing** | When the backend ends the session, the relay returns and the player is disconnected: `session.rs:573` is the last thing a session does. The fallback list applies at login only ([OPERATING.md](OPERATING.md) section 12). A kick in the configuration phase after the login (`LoginFinished`) is not seen by the login loop either | hard | high | None for a cold move (a new login and configuration on the fallback, with a loading screen, as those proxies do); the transfer module for a move without one, and a source that is gone cannot export a record, so what the player gets then is to be decided |
| Planned restart without dropping players | exists | `drain <server> --move`, `[restart] evacuate`: `evacuate.rs:51`, `evacuate.rs:98`, detector `restart.rs:103`, announcement from the server `notices.rs:257` and `meridian-sidecar/src/management.rs:178-189` | n/a | high | The transfer module for the move; the server's own `server/stopping` announcement for the early warning (not tried in a recorded run) |
| Forced hosts | exists | `meridian-config/src/lib.rs:776` (`forced_backend`), used first by `route.rs:65`; `[[forced-host]]` at `lib.rs:360` | n/a | medium | None |
| Groups of servers by kind of player | exists | `[[group]]`, `[[route]]`: `route.rs:59-111`, `lib.rs:681`. These groups pick servers for verified and unverified players; they are not groups of players | n/a | medium | None |
| MOTD per forced host | exists | `motd` on `[[forced-host]]`: `meridian-config/src/lib.rs:55`, read at `lib.rs:527`; `Config::motd_for` (`lib.rs:791`) finds the host by the rules of `forced_backend` (`lib.rs:782`, one lookup for both). The handshake's host reaches the status handler: `client.rs:192-196` (`virtual_host`, as for a login), `client.rs:599` (`serve_status`), `client.rs:648`. Without a `motd` on the host the global one is shown. Tests: `client.rs:813`, `favicon_tests.rs`. A MOTD per *route* (per kind of player) does not exist: a status request has no account | n/a | medium | None |
| Hub command and default server | exists | `/hub`, `/lobby`, `hub-backend`: `commands.rs:65`, `meridian-config/src/commands.rs:86` | n/a | high | None |
| Load balancing among lobbies | exists | `placing.rs:44` (`login`), `balance.rs:413`; roles, caps, drain ([LOAD-BALANCING.md](LOAD-BALANCING.md)). Beyond what the two proxies do natively | n/a | high | The server's own count: balancing counts the proxy's sessions today |

## 2. Server list

| Function | Status | Where in Meridian, or what is missing | Difficulty | Value | Horizon |
| --- | --- | --- | --- | --- | --- |
| Own message of the day, online count, shown maximum, sample of names | exists | `client.rs:599-657` (`serve_status`, `status_json`), `max_shown` at `client.rs:660`, `status-sample` at `lib.rs:174` | n/a | high | None |
| Total players of the network | partial | The count is the players this proxy relays: `client.rs:612` (`players_online`). With one proxy it is the total. It is not summed over the servers, nor over several proxies | easy (one proxy); hard (several, see section 6) | medium | None |
| Favicon | exists | `favicon = "file.png"`: `meridian-config/src/lib.rs:134`, `favicon.rs:41` (read at the start and at each `reload`) and `favicon.rs:57` (a 64x64 PNG of at most 16 KiB, read from its header, ending with `IEND`; an invalid file stops the start or refuses the reload). The status reply carries it as `data:image/png;base64,...`: `client.rs:653`. One picture for the whole proxy, none per forced host. Test: `client.rs:842` | n/a | medium | None |
| Ping passthrough (a server's own reply) | **missing** | The proxy answers alone, by design, so that a ping opens no second connection: `client.rs:625-632`. [README.md](../README.md) lists it as planned. The sidecar's `status` request exists (`status.rs:207` polls it) and could feed a cached answer | medium | medium | The server's own status, or a Horizon command that reports it |
| Version shown | partial | Always Horizon's: `client.rs:643-646`. Fine with one game version | easy to change | low | None |
| Query protocol | missing, out of scope | Stated out of scope in [DESIGN.md](DESIGN.md) section 14 | medium | low | None |

## 3. Commands and messages

| Function | Status | Where in Meridian, or what is missing | Difficulty | Value | Horizon |
| --- | --- | --- | --- | --- | --- |
| `/server`, `/hub`, `/lobby`, `/play` in game | exists | `commands.rs:58`, `:65`, `:78`; the words are fixed in `meridian-config/src/commands.rs:7-35`. `/server` is not completed by Tab (README) | n/a | high | The transfer module for a move without a loading screen |
| `/send` (move a player or everyone) | partial | On the console only: `console.rs:362` (`send <player\|all> <server>`), `switch` at `:351`. Not a command a player or an operator can type in game | easy | high | The transfer module |
| `/glist` (who is online, per server) | partial | Console `list` and `servers`: `console.rs:198`, `console.rs:233`. No in-game command | easy | medium | None |
| `/alert` (message to all, or to a server) | **missing** | No broadcast command. The building block exists: the relay writes a system chat line to one player (`meridian-net/src/relay.rs:306`) | easy | medium | None |
| Global chat | **missing** | Chat passes through to the player's own server and stays there; the relay "interprets nothing" (`relay.rs:1-12`). A merged chat needs the proxy to read the chat packets, which works only while secure chat is off (README, known limits) | medium | medium | None, if servers keep secure chat off |
| Private messages (`/msg`) across servers | **missing** | Same as above: no search for `msg` or `tell` in the proxy | medium | medium | None |
| Tab list merged across servers | **missing** | The player sees the list of the server they are on. `meridian-ghost/src/tab.rs:1` models the list so that a switch can replace it, but nothing merges lists. [DESIGN.md](DESIGN.md) section 14 calls it a plugin matter | hard | low | None |
| Resource pack forwarding and control | partial | Relayed unchanged in a normal login. A switch is refused when the target pushes a pack in its configuration phase: `meridian-handoff/src/configure.rs:154`. No proxy-wide pack setting. Out of scope for sending packs: [DESIGN.md](DESIGN.md) section 14 | medium | medium (high if the servers use packs) | Whether Horizon servers push packs, and how a client keeps or replaces one |
| Plugin messaging (channels) | partial | Custom payloads pass through the relay untouched; nothing in `meridian-proxy` or `meridian-net` reads a channel (no `CustomPayload`, `brand` or channel name outside the bench). No messages between servers: [DESIGN.md](DESIGN.md) section 13.4 says Horizon's `horizon:mod_net` links a server to its own players only | medium | medium | A hook in Horizon for a mod to use a channel (DESIGN.md section 16) |
| Custom commands and an extension API | **missing** | `[commands]` turns three fixed commands on or off (`meridian-config/src/commands.rs:7-15`). No trait, no events: [DESIGN.md](DESIGN.md) section 13.5 (J7), README "Extensions: planned" | hard | high | None for the proxy; hooks in Horizon for data between servers |

## 4. Players, rights and protection

| Function | Status | Where in Meridian, or what is missing | Difficulty | Value | Horizon |
| --- | --- | --- | --- | --- | --- |
| Kick | exists | Console `kick`: `console.rs:435`, `session.rs:326`. Players are told why | n/a | high | None |
| Duplicate login (refuse or replace) | exists | `on-duplicate`: `meridian-config/src/lib.rs:217`, `session.rs:211` | n/a | medium | None |
| Player limit | exists | `max-players`: `route.rs:144` (`admit`), `session.rs:174` | n/a | high | None |
| Permissions and ranks | **missing** | No permission code in the proxy, the configuration or the console (the matches for "permission" are the player's op level inside a switch, `meridian-handoff/src/prepare.rs:25`). `/server` only shows the servers open to the kind of player: `commands.rs:95` (`open_to`). README: "Permissions file: planned" | medium | high | None for the proxy's own commands |
| Bans at the proxy (name, unique id, address) | **missing** | No ban code. [DESIGN.md](DESIGN.md) section 14 plans it for J7. Servers keep their own bans | medium | high | None |
| Proxy whitelist | **missing** | No allowlist at the proxy (only `whitelist = false` in the test bench's server file: `meridian-panel/src/init.rs:243`) | easy | high | None |
| Mute at the proxy | **missing** | Needs the proxy to read chat (see global chat) | medium | low | None |
| Sync of bans and whitelist with the servers' management protocol | **missing** | The sidecar is a client of Horizon's management protocol and keeps only `server/stopping` and `server/started`: `meridian-sidecar/src/management.rs:178-189`. Vanilla's management protocol, as generally described, also has methods for the allowlist, bans and operators; whether Horizon serves them was not checked in this audit | hard | medium | Horizon serving those methods; to verify |
| Connection limit per address | exists | `connections-per-minute`: `throttle.rs:24`, used before any packet is read: `serve.rs:87`; counted on the player's address behind a load balancer (`serve.rs:76`) | n/a | high | None |
| Anti-bot beyond the rate limit | partial | Login and read timeouts (`client.rs:30`, `client.rs:180`, `client.rs:34`), `[auth] lookups-per-minute`, the hybrid mode's name checks (`login_policy.rs:208`). Missing: a cap of simultaneous connections per address, a cap of new connections per second overall, name patterns, a quarantine for a new address | medium | medium | None |
| Real address behind a load balancer | exists | PROXY protocol v1 and v2: `meridian-net/src/proxy_protocol.rs`, `serve.rs:76` | n/a | high | None |
| Online, offline and hybrid login | exists | `login_policy.rs:208`; forwarding to the servers `meridian-net/src/backend/forwarding.rs:24` | n/a | high | None |

## 5. Operation

| Function | Status | Where in Meridian, or what is missing | Difficulty | Value | Horizon |
| --- | --- | --- | --- | --- | --- |
| Reload | partial | The console command `reload`: `console.rs:462`. `listen` needs a restart | n/a | high | None |
| Reload by signal (SIGHUP) | **missing** | No signal other than Ctrl+C. A signal needs a handler other than `ctrlc`, or its `termination` feature (which also handles SIGTERM) plus a way to tell the two apart | medium | high | None |
| Clean stop on SIGTERM | **missing** | `ctrlc::set_handler` at `main.rs:89`, with `ctrlc = "3.4"` and no feature (`meridian-proxy/Cargo.toml:15`). In that crate (3.5.2 in the lock file) SIGTERM and SIGHUP are handled only with its `termination` feature, which is off here; the default covers Ctrl+C. `systemctl stop` and `docker stop` send SIGTERM. The clean stop is there (sessions closed, five seconds to end: `serve.rs:21`, `session.rs:336`); a test with a real SIGTERM would confirm it is skipped | easy | high | None |
| Log file | **missing** | Standard error only: `log.rs:52`. [OPERATING.md](OPERATING.md) section 11 says to let the service manager keep them | easy | low | None |
| TLS | partial, by design | None on the player's leg (the game protocol has none; the login is encrypted in online mode, `client.rs:352`). None on the proxy-to-server leg (plain after the login). The sidecar reaches the server's management protocol over TLS without checking the certificate: `meridian-sidecar/src/tls.rs`. The metrics page is plain HTTP on loopback | hard (proxy-to-server) | medium | Horizon accepting a protected backend leg |
| Service file and container image | **missing** | The release builds binaries for Linux and Windows x86_64 only: `.github/workflows/release.yml:41`. No unit, no Dockerfile in the repository | easy | high | None |
| Metrics and health check | exists | `metrics.rs:532`, `/metrics` and `/healthz` at `metrics.rs:510-515` | n/a | high | None |
| Remote console | **missing** | The console is standard input ([OPERATING.md](OPERATING.md) section 5.4). The panel types commands for a proxy it started | medium | medium | None |
| Updating the proxy without a cut | **missing** | The listener sets `SO_REUSEADDR` only (`serve.rs:43`); a second proxy cannot share the port, and the players of a stopping proxy are dropped. The seamless switch is not used for this | hard | medium | The transfer module, if players were to be handed to another proxy (not designed) |
| Starting and stopping servers automatically | partial | `meridian-panel` starts, stops and restarts the servers of a local test bench: `supervisor.rs:215`, `:301`, `:355`. It serves `127.0.0.1` only and is not part of the proxy. Nothing starts a server because players need one | hard (production) | medium | A way to start a Horizon server from outside |

## 6. Scale and placement

| Function | Status | Where in Meridian, or what is missing | Difficulty | Value | Horizon |
| --- | --- | --- | --- | --- | --- |
| Several proxies with shared state | **missing** | The registry, the counts, the caps, the throttle and the duplicate check are in memory in one process: `session.rs:82`, `state.rs:19`, `throttle.rs`. [OPERATING.md](OPERATING.md) section 11: counts and caps are per proxy | hard | high (when one proxy is not enough) | None |
| Queue when the network is full | **missing** | A full proxy or a lobby at its hard cap refuses with "The server is full": `route.rs:144`, `session.rs:507-517` | medium | medium | None |
| Groups of players (parties) | **missing** | No party code. The `[[group]]` of the routing is a group of servers | medium (a party that moves together uses `send`) | medium | The transfer module for a group move without a loading screen |
| Matchmaking | partial | `/play` finds a server of role `game` with room: `commands.rs:78`, `placing.rs:212`. No pairing of players, no ready check, no session that fills then closes | hard | medium | The server's own state (`accepts_transfers`, `players`) from a Horizon command that does not exist yet |
| Moving a player without a loading screen | partial | The seamless switch is built and tested on fake servers: `meridian-handoff`, `switching.rs`. No run on real servers is recorded | n/a | high | The transfer module; a real run ([TESTING-FORK.md](TESTING-FORK.md)) |
| Carrying the position at a transfer (#8735) | partial | `switch-carry-position`, off by default: `meridian-config/src/lib.rs:186`, `meridian-handoff/src/switch.rs:169` and `:200`. It needs servers that take the place in the import request, "Horizon from fork commit 46bfe5de0" ([CONFIG.md](CONFIG.md)). The repository lists #8735 among the open pull requests but does not say which one adds the place | easy (the proxy side is done) | high | The pull request that takes the place in the import; both servers in the same dimension |

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
| 1 | SIGTERM stop, then SIGHUP reload (section 5) | easy, then medium | `systemctl stop` and `docker stop` send SIGTERM; today the proxy may be killed with sessions open. SIGTERM is a feature flag of a crate already used; SIGHUP needs the two signals told apart |
| 2 | Service unit and container image, in the release (section 5) | easy | The first thing an operator asks for, and it needs no change to the proxy. Nothing else on the list has so small a cost for its value |
| 3 | Favicon and a MOTD per forced host (section 2, section 1): done, both exist | easy | Seen by every player, before they join. The status handler needs the handshake's host; the favicon is one field of the reply. Ping passthrough can come later, from the sidecar's cached state |
| 4 | Bans and whitelist at the proxy (section 4) | medium | Applied before any server is asked, in one place for the whole network. A file read at the start and at `reload`, with `ban`, `pardon` and `whitelist` on the console. The sync with Horizon's management protocol comes after, once it is known what that protocol offers |
| 5 | A permissions file (section 4) | medium | The base of everything an operator types in game. Without it every in-game command is for everyone or for nobody |
| 6 | `/send`, `/glist`, `/alert` in game (section 3) | easy, after 5 | The console already has the logic (`console.rs:198`, `:362`) and the relay can write a chat line to a player (`relay.rs:306`). It becomes a command through the permissions |
| 7 | Fallback when a backend kicks or crashes a player in game (section 1) | hard | The largest hole in resilience: a crash today drops every player on that server. Start with a cold move (a new login on the fallback, the loading screen), which needs nothing from Horizon; the seamless version waits for the transfer module |
| 8 | Queue when the network is full (section 6) | medium | The two refusal points are known (`route.rs:144`, `session.rs:507-517`). A waiting list in front of them turns a "server is full" into a wait, and is invisible to Horizon |
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
