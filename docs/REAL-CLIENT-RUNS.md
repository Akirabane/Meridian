# Runs with a real client

This page records what was seen when a real Horizon client played through Meridian. It holds
three kinds of record:

- **The reference run**: the evening of 5 November 2026, a real client over the Internet to a
  proxy on a rented server, with 250 scripted players loading the proxy at the same time
  (section 1). It has a full record sheet.
- **The fallback runs**: the same evening, a real client over the Internet while a server was
  killed, kicked the player or stopped, with the times on the servers and on the player's screen
  (section 3, runs 6 to 8).
- **Four earlier runs**, made on 4 and 5 November 2026 on one machine, which were written down in
  only one place: a public comment by Akirabane on the Horizon issue
  [horizon-rust/horizon#7484](https://github.com/horizon-rust/horizon/issues/7484#issuecomment-6644342145),
  posted on 4 November 2026 at 22:59 UTC (23:59 on the player's clock) and edited at 23:01 UTC (section 2).
  Their details were not written down at the time and are not remembered; where the comment does
  not say something, the cell says **not recorded**. They were not repeated as such; run 5 and
  runs 6 to 8 were made instead, with record sheets.

**These are the runs of one person with one client. They are not a load test of the Horizon
server, and their logs are not kept in this repository.**

## 1. The reference run: 5 November 2026, in the evening

**Real client over the Internet to a proxy on a VPS, 250 scripted players at the same time.**

### Record sheet

| | |
| --- | --- |
| Date and time | 5 November 2026. The bench was launched at 21:13:25 UTC (21:13:24.9), the first wave of scripted players came in at 21:13:27 and the run ended at 21:18:29 UTC (22:13 to 22:18 on the player's clock), 5 minutes 4 seconds from the launch |
| Client | Horizon Nightly 2026-11-05 · `66f3981` (as the client's own log says), on Windows 11 Pro, the operating system of the player's machine (build 10.0.26200). Offline account |
| Server | `horizon-server` Nightly `66f3981`. The release archive was installed with the SHA-256 `5bc6a9af...64b7` (the middle is not copied here) and its release signature was verified. A stock server: without the #7484 stack, so no seamless switch was tried |
| Proxy | Meridian at commit `e1e2a0e6` (the merge of [#102](https://github.com/Akirabane/Meridian/pull/102)) |
| Load generator | The bench of the commit `35c7e17b` (`meridian-bench`, in this repository) |
| Machine of the proxy and server | A rented virtual server: 4 vCores, 8 GB of memory, 1 Gbit/s, Ubuntu 26.04. Its place and its address are not given. The proxy, the Horizon server, the load generator and the synthetic servers all ran on it; only the client was on the player's own computer. The proxy was limited by systemd to 2 of the 4 vCores (`AllowedCPUs=0-1`) |
| Network of the player | A home line, declared as 500 Mbit/s down and 500 Mbit/s up (declared, not measured). TCP from the client to the VPS, over the Internet |
| Scripted players | 250 in all, in 5 waves of 50, one wave every 12 seconds, over the loopback of the VPS, into **synthetic servers** (stand-ins started by the bench, not Horizon). They load the proxy; they do not enter the real player's world, and the real player does not see them |

### What was observed

| | |
| --- | --- |
| The real player | Stayed connected without interruption for the 5 minutes. Only the real client goes through the backend `real` (the scripted players go to the synthetic `lobby` servers), and the proxy's log has no `left backend "real"` line since the `joined` of 21:07:48 UTC. The Horizon server was read 31 times during the run, one reading every 10 seconds, and each reading showed 1 player connected (maximum capacity 20) |
| What the player reported | "No lag, no freeze, no trouble with the TPS, no rollback." This is the player's own account, **without a video and without a frame-by-frame measure of the latency** |
| Horizon server, during the run | 20.0 ticks per second, constant. MSPT 4.6 to 6.1 ms, P95 at most 8.6 ms. 0.11 of a core |
| Proxy, during the run | Peak of 1.61 cores out of the 2 it was allowed (systemd, `AllowedCPUs=0-1`). 0 throttling by the system. At most 1019 established sockets as counted by `ss`, which counts both ends of every local connection: 250 scripted players, two hops each (player to proxy, proxy to synthetic server), two ends each, make 1000, and a few connections of the real player and of the system make up the rest |
| The 250 scripted players | All 250 entered the proxy. 0 failed |

### The two earlier runs of the same evening

Two runs came before the reference run, the same evening, and are partial: one was stopped at 45
seconds with the client connected, and in another the client left of its own accord at 21:05:53
UTC. No conclusion is drawn from them here.

### Limits of this run

- **One real client and one valid run.** Nothing was repeated, so there is no spread to quote.
- **The feeling is declared.** There is no video and no measure of the latency from one frame to
  the next. The player's ping was not recorded; it is to be added here if it is given.
- **The scripted players load the proxy, not the Horizon server.** They go to synthetic servers,
  so the Horizon server had one real player on it. The 20.0 TPS and the 0.11 of a core say
  nothing about a Horizon server with 250 players.
- **A shared machine.** The VPS is virtual and shared with other tenants of its host. The proxy
  and the load generator ran on the same machine: the load generator took about 0.9 of a core on
  average over its active window (92 %), on cores 2 and 3, apart from the proxy's cores 0 and 1.
- **A stock Horizon server.** No transfer module, so no seamless switch, no `/server` move and no
  evacuation was tried in this run. See section 2 for the moves made earlier.
- **One route.** A home line to a rented machine, one provider, one evening.
- **An offline account.** The online login and the hybrid mode were not exercised.

## 2. The four earlier runs, from the comment of 4 November 2026

These are quoted from the comment. Everything was on `localhost`, on one machine.

### The setup, as the comment tells it

| | |
| --- | --- |
| Machine | One machine, everything on `localhost`. The operating system is not recorded |
| Horizon servers | Three, called `game1` and `game2` in the comment (the name of the third is not recorded). A build "of the #7484 stack, one local commit before the heads pushed now". The only difference with the pushed head of Horizon's #8742 is a two-line fix in `crates/horizon-client/src/game/dedicated.rs`, "checked with `git diff`". The exact Horizon commit is not recorded |
| Horizon client | One, in offline mode. Its version or build is not recorded |
| Meridian | The proxy, and one sidecar per server. The commit of each run is not recorded. The permission-level bug of the first run was seen before [#70](https://github.com/Akirabane/Meridian/pull/70) (merged as `e49cf52`) and the fix was checked with the same client afterwards; the fallback runs concern the code of [#84](https://github.com/Akirabane/Meridian/pull/84) (merged as `9ad6b25`, "merged tonight" in the comment) |
| Settings named in the comment | `switch-carry-position` off (the position is not carried until Horizon's #8735 is in). Anything else, the proxy's `meridian.toml` included, is not recorded |

### The runs

| Run | Date | Configuration | What was observed | Duration measured | Limits |
| --- | --- | --- | --- | --- | --- |
| 1. Moves between servers with a real client | Not recorded. Before the comment of 4 November 2026 | The setup above. The player was moved by `/play`, `/hub`, `/server <name>`, the console's `switch` and `drain <server> --move` | The player stayed in the game, "with inventory, experience, health and hunger following, and a very slight rollback, because the position is not carried yet". No disconnection is reported | "Between about 0.6 and 1.4 seconds" for a move, in the proxy's log. How many moves, and the time by method, are not recorded | The position is not carried (see above). One client, one machine. The comment gives no counts of moves and no log |
| 2. Permission level after a move | Not recorded | As run 1 | After a move the permission level stayed at 0, and F3+F4 was refused. Meridian forwards the level since [#70](https://github.com/Akirabane/Meridian/pull/70), "and the same client confirmed it" | Not applicable | How many times it was tried after the fix is not recorded |
| 3. A server killed under the player | About 23:57 on the player's clock, on the night of 4 to 5 November 2026 ("tonight at about 23:57") | The setup above, with the player on `game2`. `game2`'s process was killed (how is not recorded) | The proxy logged `lost backend "game2" (the connection was lost ...); moved to "game1"`. The player landed on `game1` "without being disconnected, with a chat line saying so, and the proxy's counts followed". The text of that chat line is not recorded; [OPERATING.md](OPERATING.md), section 5.11 gives the wording the proxy is meant to send | Not recorded | This is a cold move: the comment describes the design as nothing exported from the lost server, "the player arrives as a new arrival". What the player saw on screen, such as the loading screen, is not recorded. The same case was run again with a record of the times, of the chat text and of the screen: run 6 in section 3 |
| 4. A server kicks the player | Right after run 3, between about 23:57 and 00:05 on the player's clock | As run 3, with the command `kick Akirabane test kick from the server console` typed at `game2`'s own console | The proxy logged `lost backend "game2" (it disconnected them: test kick from the server console); moved to "game1"`. The player landed on `game1` "without being disconnected", and the chat showed `You were disconnected from game2: test kick from the server console. You were moved to game1.` | Not recorded | As run 3. The chat line was added to the comment at about 00:05 on the player's clock, after the try. The same case was run again with the times and the screen recorded: run 7 in section 3 |

### What the comment says was not done

- **A run on a real network.** "Everything above is localhost, one machine, one client." Run 5
  (section 1) is the first over the Internet, with the limits listed there. Still not done: the
  proxy and two servers on separate machines.
- **The proxy's management connection**, which relays a server's own stop announcement, "has never
  been tried against a real Horizon". It was tried afterwards, on 2026-11-05, against three real
  servers with scripted players ([VALIDATION.md](VALIDATION.md)); not with a real client.
- **Carrying the position**, until Horizon's #8735 is in.
- **Undoing a placed arrival on the target side after a failed commit**: still the documented
  limit ([STATE-TRANSFER.md](STATE-TRANSFER.md)).
- **Syncing the proxy's bans with the servers' management protocol** was said to be possible
  later. The repository has it, off by default ([OPERATING.md](OPERATING.md), section 5.12), and it
  ran against three real Horizon servers with scripted players ([VALIDATION.md](VALIDATION.md)),
  not with a real client.
- The sidecar's `status` request is provisional: Horizon does not answer it (it replies that the
  request is unknown), and the proxy then treats the server as alive. Meridian counts the players
  itself.

## 3. The fallback runs: 5 November 2026, from 22:01 to 22:08 UTC

**A real client over the Internet, a server killed, a server that kicks and a server that stops,
recorded on the servers and on the player's screen.** These runs were made on the same evening
as the reference run (23:01 to 23:08 on the player's clock), after it. They record what the runs 3 and 4 of
section 2 did not: the times, the text shown to the player and what the player saw on screen.

### Record sheet

| | |
| --- | --- |
| Machine | The same rented virtual server as the reference run (4 vCores, 8 GB of memory). The proxy and both Horizon servers ran on it. The player's client was on the player's own computer |
| Proxy | Meridian at commit `e1e2a0e6`, with two real Horizon servers: `real`, the default, and `real2`, the only entry of the fallback list (`try = ["real2"]`). The configuration was reloaded with SIGHUP, without cutting anyone |
| Horizon servers | Two, `real` and `real2`, both Horizon Nightly `66f3981`, stock (without the #7484 stack) |
| Sidecar | None. So there is no state transfer: the fallback is a **cold move**, the player arrives as a new arrival |
| Client | Horizon Nightly `66f3981`, on Windows 11 Pro, offline account, over the Internet, one connection kept for the three runs |
| Scripted players | None during these runs |
| Source of the times | The servers' side: the notes of the test (the time of each command) and the logs of the two Horizon servers and of the proxy, which agree where they overlap. The player's side: the player's screen recordings, read frame by frame at 20 images per second (the videos are not published). The logs themselves are not kept in this repository, because they hold the player's address and name |

### The runs

| Run | Date (UTC, 5 November 2026) | Configuration | What was observed | Duration measured | Limits |
| --- | --- | --- | --- | --- | --- |
| 6. A server killed under the player (crash) | 22:01:56 | The player on `real`. `real` was killed with SIGKILL | The proxy detected the loss 15 ms after the command and logged `lost backend "real" (the connection was lost: the connection closed)`, then moved the player to `real2`. The player stayed in the game, no return to the menu, no disconnection. The chat showed `You lost the connection to real. You were moved to real2.` The player arrived on `real2` with full health and an empty hotbar: a new arrival. systemd restarted `real` about 10 s later (22:02:06) | On the servers: 0.19 s from the command to the arrival on `real2` (command at about 22:01:56.35, detection 22:01:56.366, arrival 22:01:56.543; the note of the test gives the command only to the tenth of a second, 22:01:56.3). On the screen: a brief `Reconfiguring...` screen with a Disconnect button, then `Loading terrain...`, about 0.35 s in all (plus or minus 0.05 s) | One run. The 0.19 s is measured on the servers; the 0.35 s is read from a screen recording |
| 7. A server kicks the player | 22:06:18 | The player on `real2`. The command `kick` with the reason `test kick from the server console` typed at `real2`'s own console | The proxy logged that `real2` had disconnected the player with that reason and moved them to `real`. The player stayed in the game. The chat showed `You were disconnected from real2: test kick from the server console. You were moved to real.` Back on `real`, the player found the state that `real` had saved earlier (4 hearts, one item): each server keeps its own save of the player, and nothing is carried from one to the other | On the servers: 0.19 s from the kick to the arrival on `real` (command about 22:06:18.3, kick 22:06:18.490, proxy 22:06:18.503, arrival 22:06:18.682). On the screen: the same `Reconfiguring...` then `Loading terrain...`, about 0.30 s (plus or minus 0.05 s) | One run |
| 8. A server stops on its own order (clean stop) | 22:07:48 | The player on `real`. The `stop` command typed at `real`'s console | `real` announced that it was stopping, and about 80 ms later the proxy saw it disconnect the player with the reason `multiplayer.disconnect.server_shutdown`, then moved the player to `real2`. The player stayed in the game. The chat showed `You were disconnected from real: multiplayer.disconnect.server_shutdown. You were moved to real2.` | On the servers: 0.24 s from the start of the stop to the arrival on `real2` (command about 22:07:48.7, start of the stop 22:07:48.805, proxy 22:07:48.887, arrival 22:07:49.041). On the screen: the same two screens, about 0.25 s (plus or minus 0.05 s) | One run. The proxy counts a clean stop as a kick, because the server is the one that cuts the connection. The window of about 80 ms between the announcement and the cut is the one measured on the Horizon build `02f05c1` (it is the same on this build, `66f3981`) and the one that [RESTART.md](RESTART.md) gives as the reason why `[restart] evacuate` cannot move a player ahead of a Horizon `stop` |

The proxy's own counts at the end of the three runs: fallback by kick, moved: 2; by crash, moved:
1; failed: 0. The player's TCP connection to the proxy stayed the same through the three runs.

### Defects seen

1. **The reason of a Horizon disconnection is shown as its raw translation key.** In run 8 the
   chat showed `multiplayer.disconnect.server_shutdown` instead of a translated text. The proxy
   passes on the reason it gets; turning the key into a sentence is open.
2. **`/server` lists `lobby`.** `lobby` is the stand-in server of the load bench, which was still
   in the configuration of the test; a player could type `/server lobby`. It is a setting of the
   test configuration, not behaviour of the proxy.
3. **The seamless transfer, without a loading screen, was not tested.** The Horizon servers were
   stock, without the #7484 stack, and there was no sidecar.

### Limits of these runs

- **One client, one route, one run of each.** Nothing was repeated, so there is no spread.
- **The feeling and the screen times come from a recording**, read frame by frame at 20 images
  per second (plus or minus 0.05 s). There is no measure of the latency from one frame to the
  next.
- **A cold move.** What the lost server had not saved is lost; nothing is carried from one server
  to the other. That is the design ([OPERATING.md](OPERATING.md), section 5.11), and the runs
  confirm it for health and the hotbar.
- **Two servers and one player.** No scripted player, no load, and the fallback list had a single
  entry.
- **A shared machine**, as in the reference run, and an offline account.
- **A stop of that build.** The window of about 80 ms in run 8 is that of a Horizon `stop` before its
  stop wait (`[transfer] stop_wait_seconds`, Horizon's pull request #11713, merged on 8 November 2026
  at 01:18 UTC). It is not the behaviour of a Horizon that has it.

## 4. Where the other pages point here

The pages that said that no real client was tried were reconciled with this page: the README,
[OPERATING.md](OPERATING.md) (sections 0, 5.11 and 12), [TESTING-FORK.md](TESTING-FORK.md) and
[PARITY.md](PARITY.md). Each now says what is recorded here and what is not.

As of 5 November 2026, the date of the runs above, what those pages said was not run with a real
client, and this page did not change:

- the seamless switch in a recorded run: only the public comment of section 2 describes moves with
  a real client, on one machine, and it was not repeated; the runs of sections 1 and 3 used a stock
  Horizon server, so the seamless transfer was not tried in them;
- any move of a player over a real network by the seamless switch;
- a planned restart, drain <server> --move and [restart] evacuate with a real client;
- the queue as a real client sees it while it waits, the sync of bans and whitelist with a real
  client, the hybrid and online modes, more than one real client, and long sessions.

A later page, [REAL-NETWORK-RUN.md](REAL-NETWORK-RUN.md), records the seamless switch itself with
real clients over a network (6 November 2026): one server on the proxy's machine, the other on the
clients' machine across the Internet. It says what that run did not cover.

Since then (6 and 7 November 2026), the first three items above were run with real clients over a
real network, in that page: the seamless switch, `drain --move` and the evacuation of a restarting
server (runs 1 to 6; run 7 for a stop with a wait for the evacuation, which Horizon's pull
request #11713 added, merged on 8 November 2026). It was three real clients on one PC, one session each. The queue as a
real client sees it, the sync of bans and whitelist with a real client, the hybrid and online modes
and long sessions are still not run with a real client.
