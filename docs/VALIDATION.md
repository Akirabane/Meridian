# Validating against real Horizon servers

Three features were written and tested against stand-ins for Horizon: the sync of bans and
whitelist with the servers' own lists ([OPERATING.md](OPERATING.md), section 5.12, `[access.sync]`),
the evacuation of a server that is about to restart (section 6, `[restart] evacuate`), and the
announcement of a server's stop that the sidecar hears on its management protocol
([RESTART.md](RESTART.md)). This note says, step by step, how to try each of them against three
real Horizon servers, what to expect, and leaves a column for what really happened. **Nothing in
it has been run yet**: the "Actual result" columns are empty on purpose.

The steps are manual: commands typed at the consoles of the bench, and files read. The scripts
that automate them for the bench are kept with the bench, outside this repository.

## 1. What is checked, and against what it was checked before

| Claim | Tested before against | Scenario |
| --- | --- | --- |
| A ban, pardon or allowlist change made on a server reaches the proxy's lists, and a connected player of that name is sent away | A stand-in for the management protocol that uses Horizon's methods and shapes as read in its source | A, phase 1 |
| `ban`, `pardon`, `whitelist add\|remove` at the console reach every server through its sidecar, with retries, without holding back the console or a login | The same stand-in | A, phases 2 and 3 |
| A name alone (`bans/add` with no unique id) is accepted by a real server (the open question of the sync) | Nothing | A, 2.1 and 2.5 |
| A restarted server's lists are read again and merged; nothing local is removed | The stand-in | A, 3.4 |
| The sidecar speaks to a real management socket (TLS with the server's own certificate, bearer secret) and hears `server/stopping` and `server/started` | A WebSocket stand-in, plain and TLS | C |
| With the announcement and `[restart] evacuate`, a player is moved without a cut to a server of the same role **before** the server goes | Stand-ins; the move itself (`switch`, `drain --move`) was run on this bench | B |
| Without an announcement (a kill), the fallback of #84 takes over | Run on this bench | B3 (control) |

## 2. The bench and its exact configuration

The bench is `banc-lb`: three Horizon servers (`lobby`, `game1`, `game2`), the proxy, three
sidecars, and the panel on 127.0.0.1:8140. The servers `lobby` and `game1` have the role `lobby`,
`game2` the role `game`.

**Programs.**

- The proxy and the sidecars must be built from a `main` that has #86 (`31c89f4` or later). A
  build from before it refuses `[access.sync]`. `cargo build --release -j 3 -p meridian-proxy
  -p meridian-sidecar`, and `cargo build --release -j 3 -p meridian-testkit --example real_run`
  for the scripted client. Point the `program` lines of the proxy and the three sidecars in
  `bench.toml` at the new `meridian.exe` and `meridian-sidecar.exe`.
- Horizon: the build of the #7484 stack (`02f05c1`, base `f897927ef`). Its `horizon-server.exe` has
  the management notifications (`server/stopping`, `server/started`, the lists' `added` and
  `removed`) and `HORIZON_API_DISCOVERY`; a newer build may be asked for.

**The management protocol is configured in `server.toml`, not in `server.properties`.** A Horizon
server writes `server.properties` from `server.toml` before every start, so a key added to
`server.properties` by hand is lost at the next start. With the bench stopped, add to each
server's `server.toml`:

```toml
[properties]
management-server-enabled = true
management-server-host = "127.0.0.1"
management-server-port = 25611        # lobby; game1 25612; game2 25613
management-server-tls-enabled = true  # the default: the server makes its own certificate
management-server-secret = "<40 letters and digits, a different one for each server>"
```

The secret is the bearer token and must be exactly 40 letters and digits (Horizon refuses
anything else). It stays in the bench folder. Port 0, the default, is not usable: the sidecar
needs a fixed port.

**The sidecars.** In `bench.toml`, add `"--management-properties", "server.properties"` to the
`args` of each of the three sidecars. Their working folder is the server's, so the path is
relative. They read the host, port, secret and TLS flag from that file at every connection. The
host must be a loopback address.

**The proxy.** In `proxy\meridian.toml`:

```toml
# in each [[backends]], after sidecar-secret-file:
sidecar-notices = true

[access.sync]
from-servers = true
to-servers = true

[restart]
evacuate = true
```

Check it with `meridian --check meridian.toml`. A change of `[access.sync]` or `[restart]` is
applied by `reload` at the proxy's console, without a restart.

**Do not add `[drain-before-stop]` to `bench.toml`.** With it the panel tells the proxy before it
stops a server, which would hide whether the server's own announcement works.

**Ports.** Already used by the bench: 8140 (panel), 25565 (proxy), 25580-25582 (sidecars),
25601-25603 (servers). New: 25611-25613 (the servers' management protocol, loopback). Each Horizon
server also opens a control endpoint on a random loopback port (`.horizon/engine.json`), as
before. The scripted client listens on nothing.

**Optional: no call to Mojang's profile service.** A bare name given to `bans/add` is looked up in
the server's user cache, then at the profile service (`HORIZON_API_DISCOVERY`, default
`discovery.minecraftservices.com`), and only then made an offline player (section 6). To see the
difference, start the panel once with the variable set to a closed local port:
`$env:HORIZON_API_DISCOVERY = 'http://127.0.0.1:9/'`, then the panel as usual.

## 3. Load

An estimate, to be replaced by what the panel shows (memory and processor of each card):

| | Estimate |
| --- | --- |
| Memory | 3 Horizon servers (a supervisor and a game process each), the proxy, 3 sidecars and the panel: about 4 to 7 GB, mostly the three game processes with a world loaded. Not measured on this build. |
| Cores | Steady: under one core for the whole bench, the players being only the scripted client. Peaks of a few cores at a server start (20 to 60 s) and at a move. |
| Rebuild | `cargo build --release -j 3`: 3 cores for several minutes. |
| Duration | Setup and start about 5 min. A about 20 min by hand (the restarts of game1 and game2 are most of it), B about 10, C about 10. |
| Players | At most 2 scripted clients at once, one connection each. No bots. |

## 4. Procedure

1. With the bench stopped, make the changes of section 2. Start the bench: `meridian-panel.exe
   bench.toml`, open http://127.0.0.1:8140, "Start all", wait for the 7 cards to be online.
2. **Check the chain before anything else.** At the proxy's console, `servers` lists the three
   servers; `list` says `no players online`; the proxy's log has no line saying a sidecar "does
   not listen" or "is not connected" to the server's management protocol. On the machine, each
   sidecar holds one established connection to its server's port 25611-25613
   (`Get-NetTCPConnection -State Established -RemotePort 25611`, whose `OwningProcess` is the
   sidecar's pid on the panel page). If a sidecar has none, nothing below can work: read its
   card's log.
3. Players are only the scripted client: `real_run.exe run --port 25565 --name vt_alpha --seconds
   120`, no window. It prints a last line `RESULT clean|disconnect <text>|closed|error` with
   `moves=N`, the times the server took it back to the configuration phase (a move). Use names
   that start with `vt_` and are lower case (section 6, item 3).
4. What a server holds is read from its folder (`banned-players.json`, `banned-ips.json`,
   `whitelist.json`; Horizon writes them as the lists change) and from its console. What the proxy
   holds is `banlist` and `whitelist list` at its console.
5. Times: every line of the proxy's log starts with a UTC time to the millisecond. A duration is
   the difference of two lines, or of a line and the instant a command was typed (note it).
6. Between scenarios, lift what was made: `pardon` and `whitelist remove` at the proxy,
   `pardon`, `pardon-ip` and `whitelist remove` at each server's console, and kill the scripted
   clients. Put `[access.sync]` and `[restart] evacuate` back on and `reload`.
7. Stop the bench ("Stop all", close the panel) and check that no `horizon-server`, `meridian` or
   `meridian-sidecar` is left in the task manager.

## 5. Scenarios

"Expected" is what the stand-ins and the sources lead to. "Actual result" is for whoever runs it.
`<server>:` before a line means that line is typed at that server's console in the panel,
`proxy:` at the proxy's.

### A. Sync of bans and whitelist

Phase 1 runs with `from-servers` on and `to-servers` off, phase 2 the other way round, phase 3
with both on (`reload` after each edit of `meridian.toml`).

| ID | What | Steps | Expected | Actual result |
| --- | --- | --- | --- | --- |
| 1.1 | Ban on a server, player connected | Log `vt_alpha` in; `<its server>: ban vt_alpha griefing`; `proxy: banlist` | The proxy's `banlist` has the offline id of `vt_alpha` with the reason `banned on <server>: griefing`, within a few seconds. `proxy: list` no longer shows the player. The log may show the fallback moving them to another server first, then the kick | |
| 1.2 | The banned player comes back | Log `vt_alpha` in again | `RESULT disconnect` with the ban text | |
| 1.3 | Pardon on the server | `<server>: pardon vt_alpha` | The proxy lifts that entry; the player can log in again | |
| 1.4 | Pardon on a server does not lift the proxy's own ban | `proxy: ban vt_beta mine`; `game1: ban vt_beta`; `game1: pardon vt_beta` | The entry `banned on game1` goes; the one with the reason `mine` stays | |
| 1.5 | A name the server has never seen | `game2: ban vt_gamma`, then `game2: pardon vt_gamma` | The proxy has it and lifts it | |
| 1.6 | Allowlist | `game2: whitelist add vt_delta`, then `game2: whitelist remove vt_delta` | `proxy: whitelist list` has it after the add and keeps it after the remove (by design) | |
| 1.7 | Address | `lobby: ban-ip 203.0.113.9 test`, then `lobby: pardon-ip 203.0.113.9` | The proxy bans the address, then lifts it | |
| 1.8 | A name with a capital (observation) | Log `vt_Mixed` in; `<its server>: ban vt_Mixed` | Note what happens. The server bans the id of the **lower-case** name; the player's id is the one of `vt_Mixed`, so the proxy's ban may not stop them | |
| 1.9 | State read again, nothing local removed | Both off, `reload`; `proxy: ban vt_local keep-me`; `game2: ban vt_merge`; `from-servers` on, `reload` | `vt_merge` is not taken while off, is taken once on again; `vt_local` stays and is not on game2 | |
| 2.1 | Bare name pushed to all servers | `proxy: ban vt_epsilon spam`; read each server's `banned-players.json` | Each server holds it with source `Meridian`, reason `spam`, and the offline id of the lower-case name, within a few seconds. The proxy's log says `sent the ban of vt_epsilon to backend "<x>"` for each | |
| 2.2 | Pardon pushed | `proxy: pardon vt_epsilon` | Gone from the three servers | |
| 2.3 | Address and range | `proxy: ban 203.0.113.9`; `proxy: ban 203.0.113.0/24` | The address reaches `banned-ips.json` of the three servers; the range stays at the proxy and the console says `not sent to the servers` | |
| 2.4 | Whitelist pushed | Log `vt_zeta` in; `proxy: whitelist add vt_zeta`, then `whitelist remove vt_zeta` | In `whitelist.json` of the three servers, then out. Note whether the server kicked the connected player ("Kick unlisted players") | |
| 2.5 | A name Horizon may not resolve (observation) | `proxy: ban vt_this_name_is_far_too_long toolong` | Note the proxy's log and whether each server holds it. A name Horizon cannot resolve is ignored with no error (section 6) | |
| 3.1 | No loop | Both on; `proxy: ban vt_loop looptest`; wait 12 s; `proxy: pardon vt_loop` | One entry on each server, one entry at the proxy with the console's reason, nothing taken back in the proxy log (`banned on`), nothing sent again; the pardon removes it everywhere once | |
| 3.2 | Idempotence | Ban the same name twice on one server (`game1: ban vt_idem` twice); `game1: pardon vt_never_banned`; `proxy: pardon vt_idem` | Nothing fails. One entry. The proxy has it once. The pardon removes it everywhere and the removal notice finds nothing left to lift. Note whether the second ban is announced again | |
| 3.3 | A server that is down | Panel: stop `game2`; `proxy: ban vt_slow slow` (note how long the console takes); log `vt_ok` in; start `game2` | The console answers at once; the login works; lobby and game1 have the ban at once; the log says game2 cannot be reached and will be tried again; game2 has it after it is back (retries at 2, 5, 15, 30, 60, 120 s). Note the delay from game2's card turning online | |
| 3.4 | A server that restarts | `from-servers` on, `to-servers` off; `proxy: ban vt_local keep-me`; stop `game1`; with it stopped, `horizon-server.exe ban vt_down "banned while down" --dir <game1 folder>`; start `game1` | After game1 is back and its sidecar reconnected, the proxy has `vt_down` (`banned on game1`); the proxy-only ban stays. The CLI syntax is not checked: if it fails, add the line to `banned-players.json` by hand | |
| 3.5 | Both off | Both off, `reload`; `proxy: ban vt_off`; `game2: ban vt_off2` | The console does not mention the servers; neither list changes | |

### B. Evacuation before a restart

`lobby` and `game1` have the role `lobby`; `game2` is the only one with the role `game`. So the
evacuation to "a server of the same role" is tried on the lobby pair (B1); `game2` shows what
happens with nowhere to go (B2). `[restart] evacuate = true` for B1 to B3.

| ID | What | Steps | Expected | Actual result |
| --- | --- | --- | --- | --- |
| B1 | Clean stop, player moved first | Log `vt_b1` in with `--seconds 60` (it lands on `lobby` or `game1`); note the time; in the panel, stop that server (the panel types `stop` at its console) | The proxy logs `backend "<x>" is going down: 1 moved, 0 stayed` and `vt_b1 moved from <x> to <y> in N ms`, with no `lost backend` line. The client ends `RESULT clean` with `moves=1`. Note: stop typed to the announcement, the move (N), and whether the move finished before the server's card went down (it should) | |
| B2 | Nowhere to go | Log `vt_b2` in; `proxy: switch vt_b2 game2`; stop `game2` | `vt_b2 stays on game2: no other server of role game has room`; when the server drops them the fallback (#84) moves them to a lobby | |
| B3 | Kill, no announcement (control) | `proxy: switch vt_b3 game2`; end game2's supervisor process in the task manager | The fallback moves the player; the detector finds the silence after `silent-rounds` (3) rounds of 3 s; nothing is announced | |
| B4 | `evacuate = false` | `evacuate = false`, `reload`; log a player in; stop their server | `going down (it announced its stop): its players stay on it`; the fallback moves the player | |
| after each | The server returns | Start it in the panel | Card online, the proxy logs `backend "<x>" is back`, `proxy: servers` no longer says draining | |

### C. The announcement on the real socket

Turn `[restart] evacuate` off for C so that the log says why the proxy drained the server.

| ID | What | Steps | Expected | Actual result |
| --- | --- | --- | --- | --- |
| C1 | The sidecars hold their connection | `Get-NetTCPConnection` as in section 4, twice, 12 s apart | The same local port both times, owned by each sidecar: TLS and the secret were accepted and the connection holds | |
| C2 | `server/stopping` through the sidecar | Stop `game1` in the panel | `proxy` log: `backend "game1" is going down (it announced its stop)` (and not `it stopped answering`). Note stop to that line, and that line to the end of the card | |
| C3 | `server/started` with the supervisor alive | `horizon-server.exe cmd stop --dir <game1 folder>` (stops the game only); then `game1: start` | The supervisor and the sidecar's connection stay the same; the proxy logs the stop, then `is back`. A gap of a few seconds or less between the game coming up and `is back` points to the notice; a longer one, to the probe | |
| C4 | The panel restarts a server | Restart `game1` in the panel | The sidecar reconnects on a new connection. Note the delay from the card being online again | |

## 6. Where Horizon is expected to differ from the stand-ins

Read from Horizon's sources (`origin/main`: `horizon-server/src/management*.rs`,
`horizon-client/src/game/dedicated/management.rs`, `horizon-server-files/src/profiles.rs` and
`user_cache.rs`). None of it has been seen running.

1. **The management keys live in `server.toml`** (section 2). A key added to `server.properties`
   only works until the next start.
2. **A bare name is resolved by the server, in this order:** its user cache, then Mojang's profile
   service (up to 15 s, from the game's own thread), then, if the server is offline, the offline
   player. On a bench with no network, or slow, a `bans/add` by name could take longer than the
   sidecar's 10 s call limit: the proxy would count it as "not reached" and try again, while the
   game thread waits. `HORIZON_API_DISCOVERY` pointing at a closed port makes it fail at once.
3. **The offline id is made from the lower-case name.** A player `Alex` has the id of `Alex`; the
   server bans the id of `alex`. A ban by name on a server, or sent by the proxy, may not stop a
   player whose name has a capital. Scenario 1.8 shows it. The steps use lower-case names.
4. **A name the server cannot resolve is ignored and the call still answers ok.** The proxy logs
   `sent ... to backend` and has no way to know the list did not change. Scenario 2.5 looks for it.
5. **A server's ban kicks the player** ("banned") on that server. The proxy's fallback (#84,
   `on-kick`) may move the kicked player to another server for a moment before the proxy's own ban,
   taken from the notice, sends them away. The end state is the same; the sequence may not be.
6. **`allowlist/remove` runs "Kick unlisted players"** on the server. The bench servers do not use
   the allowlist (`whitelist = false`), so nobody should be kicked; scenario 2.4 checks.
7. **The window of the announcement.** `server/stopping` is sent when the shutdown begins, but a
   vanilla-derived shutdown closes the listener and disconnects players early. Whether the proxy
   has time for a move (a few hundred ms to over a second, plus up to half a second between two
   players, plus the sidecar's long poll) before the connection drops is the main thing B1
   measures. If the move loses the race, the fallback still catches the player, and the log will
   say `lost backend`.
8. **A `stop` typed at the supervisor ends it.** The panel's `stop` ends the supervisor and the
   game, so the sidecar loses its connection and no `server/started` follows; the restart ends
   when the server answers again. Only a stop of the game alone (C3) keeps the supervisor and so
   can send `started` on the same connection. After a panel restart the sidecar reconnects with a
   wait of 1 s growing to 30 s and may miss `started`.
9. **TLS.** The server makes a self-signed certificate (a keystore it writes into its folder) the
   first time. The sidecar accepts any certificate through `rustls` with a custom verifier;
   whether the real certificate's key type and signature scheme are among those it offers is
   first seen here.
10. **Idempotence.** Horizon's list `add` replaces an existing entry; whether it announces
    `added` again for an unchanged entry, and whether `remove` of an absent entry answers ok or
    announces `removed`, is read in 3.2.
11. **The Horizon build.** `02f05c1` is the #7484 stack on `f897927ef`. If a method or a
    notification differs from `origin/main` (names, parameter shapes, `params` as a one-element
    array), the first symptom is a `Method not found` or an `Invalid params` answer in a proxy
    warning.
12. **The CLI.** `horizon-server ban <name> --dir <folder>` and `horizon-server cmd stop --dir
    <folder>` follow the documentation (`docs/server.md`), not a run.
