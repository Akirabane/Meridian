# Runs with a real client

This page records what was seen when a real Horizon client played through Meridian. It holds two
kinds of record:

- **The reference run**: the evening of 5 November 2026, a real client over the Internet to a
  proxy on a rented server, with 250 scripted players loading the proxy at the same time
  (section 1). It has a full record sheet.
- **Four earlier runs**, made on 4 and 5 November 2026 on one machine, which were written down in
  only one place: a public comment by Akirabane on the Horizon issue
  [horizon-rust/horizon#7484](https://github.com/horizon-rust/horizon/issues/7484#issuecomment-6644342145),
  posted on 4 November 2026 at 22:59 UTC (23:59 in Paris) and edited at 23:01 UTC (section 2).
  Their details were not written down at the time and are not remembered; where the comment does
  not say something, the cell says **not recorded**. They were not repeated; run 5 was made
  instead, with a record sheet.

**These are the runs of one person with one client. They are not a load test of the Horizon
server, and their logs are not kept in this repository.**

## 1. The reference run: 5 November 2026, in the evening

**Real client over the Internet to a proxy on a VPS, 250 scripted players at the same time.**

### Record sheet

| | |
| --- | --- |
| Date and time | 5 November 2026, from 21:13:24 to 21:18:29 UTC (22:13 to 22:18 in Paris), 5 minutes 5 seconds |
| Client | Horizon Nightly 2026-11-05 · `66f3981` (as the client's own log says), on Windows 11 Pro, the operating system of the player's machine (build 10.0.26200). Offline account |
| Server | `horizon-server` Nightly `66f3981`. The release archive was installed with the SHA-256 `5bc6a9af...64b7` (the middle is not copied here) and its release signature was verified. A stock server: without the #7484 stack, so no seamless switch was tried |
| Proxy | Meridian at commit `e1e2a0e6` (the merge of [#102](https://github.com/Akirabane/Meridian/pull/102)) |
| Load generator | The bench of the commit `35c7e17b` (`meridian-bench`, in this repository) |
| Machine of the proxy and server | A rented virtual server (OVH): 4 vCores, 8 GB of memory, 1 Gbit/s, Ubuntu 26.04, in Warsaw. Its address is not given. The proxy, the Horizon server, the load generator and the synthetic servers all ran on it; only the client was on the player's own computer. The proxy was limited by systemd to 2 of the 4 vCores (`AllowedCPUs=0-1`) |
| Network of the player | A home line in France, declared as 500 Mbit/s down and 500 Mbit/s up (declared, not measured). TCP from the client to the VPS, over the Internet |
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
- **One route.** France to Warsaw, one home line, one provider, one evening.
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
| 3. A server killed under the player | About 23:57, Paris time, on the night of 4 to 5 November 2026 ("tonight at about 23:57 (Paris)") | The setup above, with the player on `game2`. `game2`'s process was killed (how is not recorded) | The proxy logged `lost backend "game2" (the connection was lost ...); moved to "game1"`. The player landed on `game1` "without being disconnected, with a chat line saying so, and the proxy's counts followed". The text of that chat line is not recorded; [OPERATING.md](OPERATING.md), section 5.11 gives the wording the proxy is meant to send | Not recorded | This is a cold move: the comment describes the design as nothing exported from the lost server, "the player arrives as a new arrival". What the player saw on screen, such as the loading screen, is not recorded |
| 4. A server kicks the player | Right after run 3, between about 23:57 and 00:05 Paris time | As run 3, with the command `kick Akirabane test kick from the server console` typed at `game2`'s own console | The proxy logged `lost backend "game2" (it disconnected them: test kick from the server console); moved to "game1"`. The player landed on `game1` "without being disconnected", and the chat showed `You were disconnected from game2: test kick from the server console. You were moved to game1.` | Not recorded | As run 3. The chat line was added to the comment at about 00:05 Paris time, after the try |

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

## 3. What this changes in the other pages

The pages below were written before this one. Each sentence quoted here says or implies that no
real client was tried, and each proposed sentence replaces it. None of these pages has been
edited yet. None of the proposed sentences claims more than is written in sections 1 and 2.

| Page and place | Now says | Proposed |
| --- | --- | --- |
| README, "Moving a player to another server" | "no run on real servers is recorded yet" | "Moves between three real Horizon servers with a real client are recorded ([docs/REAL-CLIENT-RUNS.md](docs/REAL-CLIENT-RUNS.md), section 2) and were not repeated; the seamless switch has not been run on a real network" |
| README, the list of what is not measured | "No figure comes from a real Horizon server under load (...), real players, a real client or a network" | "The figures of a real client come from one player: one run over the Internet to a proxy on a rented server, with 250 scripted players loading the proxy ([docs/REAL-CLIENT-RUNS.md](docs/REAL-CLIENT-RUNS.md)). No figure comes from a Horizon server under load or from several real players" |
| README, known limits | "The switch has not been tried on a real network." | "The switch was run with a real client on one machine, not on a real network. A real client over the Internet was tried once, without a move between servers ([docs/REAL-CLIENT-RUNS.md](docs/REAL-CLIENT-RUNS.md))" |
| README, known limits | "this has not run with a real client" (a server that stops or kicks) | "this ran with a real client on one machine, twice (a killed server and a kick), without a record of the loading screen ([docs/REAL-CLIENT-RUNS.md](docs/REAL-CLIENT-RUNS.md), section 2)" |
| OPERATING.md, section 0, first point | "Meridian has not run on a real network yet ... Nothing was measured with real servers, a real network or real players." | "Meridian ran once over the Internet: a real client in France to a proxy on a rented server in Warsaw, with 250 scripted players, for 5 minutes ([REAL-CLIENT-RUNS.md](REAL-CLIENT-RUNS.md)). That is one player, one run, a stock Horizon server and a shared machine; it is not a production figure. Nothing else was measured on a network" |
| OPERATING.md, section 0, switch point | "no run on real servers is recorded in the repository yet" | "moves with a real client between three real servers are recorded ([REAL-CLIENT-RUNS.md](REAL-CLIENT-RUNS.md), section 2) from a one-machine run that was not repeated, and [TESTING-FORK.md](TESTING-FORK.md) is the procedure for a recorded one" |
| OPERATING.md, section 5.11, "What the player sees" | "(no real client was tried)" | "(a real client was tried on one machine; what it saw on screen was not recorded, [REAL-CLIENT-RUNS.md](REAL-CLIENT-RUNS.md))" |
| OPERATING.md, section 6 (the freeze/reconfigure sentence) | "this was not run with a real client" | Keep the claim and say why it still holds: "this was not run with a real client: none of the runs of [REAL-CLIENT-RUNS.md](REAL-CLIENT-RUNS.md) covers a planned restart" |
| OPERATING.md, section 8, first and second points | "Nothing has run on a real network" / "No run of it on real servers is recorded yet" | "One run over the Internet is recorded (a single real client, [REAL-CLIENT-RUNS.md](REAL-CLIENT-RUNS.md), section 1); the load figures of BENCH.md are still local" / "Moves on real servers are recorded from one machine ([REAL-CLIENT-RUNS.md](REAL-CLIENT-RUNS.md), section 2)" |
| OPERATING.md, section 8, fallback point | "This has not run with a real client." | "This ran with a real client on one machine, for a killed server and for a kick ([REAL-CLIENT-RUNS.md](REAL-CLIENT-RUNS.md), section 2); the loading screen was not recorded" |
| TESTING-FORK.md, "Where things stand" | "tested against fake servers only; this bench is where it meets real ones for the first time" | "tested against fake servers and, with a real client on one machine, against three real servers once ([REAL-CLIENT-RUNS.md](REAL-CLIENT-RUNS.md)); this bench is the procedure for a recorded run" |
| PARITY.md, "Fallback when a backend kicks or crashes a player" (status cell) | "exists, not run with a real client" | "exists, run with a real client on one machine (a killed server and a kick, [REAL-CLIENT-RUNS.md](REAL-CLIENT-RUNS.md)); not repeated" |
| PARITY.md, "Moving a player without a loading screen" | "No run on real servers is recorded" | "Run with a real client on one machine, not repeated and without the position ([REAL-CLIENT-RUNS.md](REAL-CLIENT-RUNS.md)); not on a real network" |
| PARITY.md, list item 7 | "Done, as a cold move; not run with a real client" | "Done, as a cold move; run with a real client on one machine, not repeated" |

Whoever edits the pages should check each sentence against its page before replacing it: the
"Now says" column quotes the pages as they stand on the day this page was written, and some of
the sentences are about more than one thing.
