**Local measurements, on a shared Windows machine, over loopback, against synthetic servers, with
no outside comparison. They say what this proxy costs here; they are not its performance in
production, and they are not a promise.**

# Load baseline

This is the first measurement of how Meridian scales, made before any decision about its
architecture: DESIGN.md section 17.4 says the proxy keeps a thread or two for each player and "holds a few
thousand connections", and that the limit is "a figure of the baseline, not a guess". These are the
figures up to 500 connected players. Nothing in the proxy was changed to take them.

The harness is `meridian-bench load`, in the `meridian-bench` crate next to the older latency
benchmark ([BENCH.md](BENCH.md)). The raw figures are the CSV files in [baseline/](baseline/), one row per
step, with every column the harness reads; the tables below show the main ones.

## In short

Up to 500 simultaneous players, one proxy, this machine:

1. **Memory.** About 360 KiB of resident memory for each player who has only logged in, and 410 to 640 KiB
   (455 at 500 players) for a player who was sent a world of 169 columns and 30 entities (the proxy's own memory, in total 232 MiB for 500 players
   at rest, 237 MiB with 500 playing).
2. **Threads.** Exactly three for each player, and twelve besides: 1512 at 500 players. DESIGN.md section 17.4 expected two.
3. **Processor.** At rest, 0.58 % of one core for 500 players. Playing (20 small packets and 2 chunks a second
   down, 20 packets a second up), 0.27 % of a core for each player with compression as a
   server asks for it by default, 0.19 % with none: 1.33 cores for 500 players.
   The figure per player was the same at 100, 250 and 500.
4. **Latency.** The proxy added 75 µs to the median round trip and 116 µs to the
   99th percentile at 500 players (138 µs and 223 µs through it; DESIGN.md proposes under 1 ms and 5 ms).
5. **Logins.** A burst of players who each join a world is slow: 205 logins a second, with a median of
   1,432 ms for 500 at once, because each join sends 2.7 MB through the proxy. With nothing to send
   the proxy takes 4,418 a second, and 2,632 connections a second that come and go.
   Moving a player to another server took 1,892 ms when 50 were moved at once (583 ms for 10), and the
   other players' latency at the 99th percentile stayed under 0.35 ms.

No player failed to log in, none was dropped, and the proxy stayed up in every step.

## What was measured, and how

The load is one program, `meridian-bench`, started beside the real `meridian` program, which is the
release build and runs as a process of its own. The program holds:

- **Synthetic servers.** They log players in as a Horizon server behind the proxy does (the forwarding of
  the player's information, signed with the shared secret, and compression), send the configuration
  phase (40 KB of registry data), then a world: Login, `(2r + 1)^2` chunk columns of 2 to 30 KB each
  (radius `r` is 6 here, so 169 columns and about 2.7 MB, which compress to about a third), 30 entities.
  After that they stream what a busy server does to each player: 20 small packets a second of 50
  to 200 bytes (six in ten are Teleport Entity packets, which the proxy's ghost reads and keeps; the others are opaque), two chunk packets a second
  and a keep-alive every 15 s. They answer a measuring packet at once. They do not simulate anything.
- **Bots.** Each one is a thread that logs in offline, answers keep-alives, confirms every teleport, and
  sends 20 movement packets a second and, in the active scenarios, two measuring packets a second whose
  round trip it times. A bot reads everything it is sent and decodes only what it must answer.
- **A reading of the proxy process once a second:** resident memory, committed memory, threads, and
  processor time (Windows counts the processor cycles of the process, turned into time: an estimate, see
  [BENCH.md](BENCH.md)). The same reading is the status line the run prints each second.

Each scenario is run for 100, 250 and 500 bots, one step after the other, each with a new proxy:

1. **Idle.** The bots log in all at once and hold the connection for 30 s. It is run twice: with the world above and with an empty world
   (login only), to tell what a player costs from what their world costs.
2. **Active.** The same bots play for 30 s with the stream above. First they are run against the synthetic server alone, for 20 s, then through the
   proxy; the round trip added by the proxy is the difference. This is done with three settings of
   compression: *same* (the server and the proxy both compress from 256 bytes, which is
   Horizon's default), *server-off* (the server does not compress, the proxy does toward the players)
   and *both-off*.
3. **Moves.** 10 % of the bots, all at the same moment, ask `/server east`, which the proxy answers by a
   switch through the real sidecar: the proxy talks to a sidecar (the project's own server code) that
   talks to a stand-in for Horizon's control endpoint, which plays the held player as the test kit's fake
   target does. The two servers' worlds differ in every chunk and entity, so each move sends the player
   the whole world of the other. The other 90 % keep playing.
4. **Churn.** The same number of bots as there are concurrent connections connect, stay about 100 ms and leave, again and again,
   until 3,000 connections have been made; the proxy's connection limit is off in the test configuration, as in the older benchmark.

The whole run is `meridian-bench load --players 100,250,500 --seconds 30 --direct-seconds 20 --out-dir docs/baseline --label "no client"`,
and takes about 30 minutes. It runs on a machine that has other things open: the program reads how busy
the machine was before the run and the free memory (it stayed above 29 GiB), and stops itself if
free memory falls under 16 GiB, if the proxy is gone, if more than a tenth of the bots fail or are
dropped, or if its own reading is starved; a run that lasts more than `--max-total-seconds` (1,800 s unless raised) is stopped, and cut a minute later. A step has at most 500 bots and 60 s unless
`--max-bots` and `--max-seconds` are raised.

### The machine

| | |
| --- | --- |
| System | Windows 11 (build 26200) |
| Processor | Intel Core i9-14900KF, 32 logical cores |
| Memory | 63.8 GiB, about 33 GiB available before the run |
| Load before the run | about 165 % of one core in use, out of 3,200 (other programs were open) |
| Proxy | commit `4952747`, release build; `meridian-bench` itself, which holds the bots and the servers, took 255 MiB at 500 bots playing |
| Run | 5 November 2026. The whole run was made twice; the files in `baseline/` are the second. Processor and memory agree within about 10 % between the two, the login times of a burst less |

The processor time is read by cycles, an estimate. The bots and the synthetic servers run on the same
cores as the proxy: at 500 players the program took about 1.3 cores for the proxy and 1.7 for the swarm, out of 32 logical
cores, so the machine was never short of cores in these steps.

## Results

### 1. Idle: players who hold a connection

| Bots | World | Logged in | Login p50 | Login p99 | Logins/s | Proxy memory (MiB) | Per bot (KiB) | Threads | CPU at rest | Keep-alives sent / answered |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 100 | radius 6, 30 entities | 100 of 100 | 62 ms | 300 ms | 307 | 73 | 640 | 312 | 0.29 % | 200 / 200 |
| 100 | empty | 100 of 100 | 12 ms | 16 ms | 4,417 | 46 | 363 | 312 | 0.30 % | 200 / 200 |
| 250 | radius 6, 30 entities | 250 of 250 | 619 ms | 1,390 ms | 168 | 132 | 499 | 762 | 0.40 % | 500 / 500 |
| 250 | empty | 250 of 250 | 25 ms | 37 ms | 3,763 | 99 | 364 | 762 | 0.39 % | 500 / 500 |
| 500 | radius 6, 30 entities | 500 of 500 | 1,432 ms | 2,183 ms | 205 | 232 | 455 | 1512 | 0.58 % | 1094 / 1094 |
| 500 | empty | 500 of 500 | 58 ms | 80 ms | 4,418 | 186 | 360 | 1512 | 0.52 % | 1000 / 1000 |

- Memory is the proxy's resident memory at the end of the join, with the proxy's own start (10 MiB) in it.
  The bot-by-bot figure falls with more bots because the 10 MiB is shared out and the allocator reuses memory.
- Nothing is kept for a player but their session: the empty world (bare) costs 360 to 363 KiB for each player at every step, which is mostly
  the three threads' stacks and buffers. The world's own share, the ghost, is the difference between the two rows of a step: about 93 KiB for each player at 500 (the ghost
  keeps a hash of each column and the entities, not the chunk data).
- At rest the proxy used at most 0.90 % of one core for 500 players, in its busiest second. All keep-alives were answered.
- A burst of logins is slow when each login brings a world: the proxy decompresses and compresses 2.7 MB for each
  one, and a burst of 500 spent about 1.4 s at the median. With no world the 99th percentile was 80 ms.

### 2. Active: players who move, and a stream to each

| Bots | Compression | Direct RTT p50 | Proxy RTT p50 | Added p50 | Proxy RTT p99 | Added p99 | Proxy CPU (% of a core) | Per bot | Memory per bot (KiB) | Down (MB/s) |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 100 | same | 67 µs | 125 µs | +58 µs | 197 µs | +64 µs | 26.9 | 0.269 % | 590 | 3.5 |
| 100 | server-off | 67 µs | 126 µs | +59 µs | 206 µs | +70 µs | 27.0 | 0.270 % | 629 | 3.5 |
| 100 | both-off | 69 µs | 125 µs | +56 µs | 209 µs | +69 µs | 18.9 | 0.189 % | 439 | 3.5 |
| 250 | same | 65 µs | 131 µs | +66 µs | 239 µs | +122 µs | 66.3 | 0.265 % | 479 | 8.7 |
| 250 | server-off | 61 µs | 125 µs | +64 µs | 185 µs | +74 µs | 63.8 | 0.255 % | 583 | 8.7 |
| 250 | both-off | 62 µs | 124 µs | +62 µs | 185 µs | +80 µs | 45.5 | 0.182 % | 450 | 8.7 |
| 500 | same | 63 µs | 138 µs | +75 µs | 223 µs | +116 µs | 132.6 | 0.265 % | 464 | 17.4 |
| 500 | server-off | 62 µs | 136 µs | +74 µs | 271 µs | +166 µs | 126.8 | 0.254 % | 550 | 17.4 |
| 500 | both-off | 66 µs | 143 µs | +77 µs | 219 µs | +110 µs | 96.1 | 0.192 % | 415 | 17.4 |

- The latency is measured on the measuring packets, whose round trip goes through the same connection as
  the stream, so a measuring packet waits behind the chunk that is being sent. 30,047 round trips were measured at 500 players.
  The direct figure is a bot against the server on the same machine, with the same load.
- **The processor cost of a player is flat: 0.25 to 0.27 % of a core with compression on, 0.18 to 0.19 with it off, at every step.**
  *Same* and *server-off* cost the same: the relay writes each side's framing again whatever the server asks, and decompressing is cheap next to
  compressing (see the micro-measures). What saves processor time is not compressing toward the players.
- Resident memory per player while playing is 415 to 630 KiB, as at rest, and differs between runs by what the allocator kept.

### 3. Moves: players moved to another server while the others play

| Bots | Moved at once | Move p50 | Move p95 | Move max | Prepare | Freeze | Hand over | Delta | Proxy memory before → peak (MiB) | Peak extra per mover | Other bots' RTT p99, quiet → moving |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 100 | 10 of 10 | 583 ms | 612 ms | 612 ms | 424 ms | 2 ms | 84 ms | 2,670 KiB, 169 chunks, 31 entities | 58 → 65 | 0.7 MiB | 534 → 241 µs |
| 250 | 25 of 25 | 939 ms | 1,035 ms | 1,036 ms | 752 ms | 2 ms | 114 ms | 2,716 KiB, 169 chunks, 31 entities | 130 → 147 | 0.7 MiB | 261 → 245 µs |
| 500 | 50 of 50 | 1,892 ms | 2,137 ms | 2,162 ms | 1,635 ms | 3 ms | 199 ms | 2,701 KiB, 169 chunks, 31 entities | 235 → 380 | 2.9 MiB | 266 → 347 µs |

- A move is mostly its prepare step: the proxy logs the player in on the target and takes that server's whole world before it freezes the
  player on the old one. The freeze is 2 to 3 ms in every step and the commit 1 ms. Prepare is about 0.3 s alone, and grows with the number of
  moves at once, with the proxy at 480 % of a core at the peak with 50 moves: the move's time is the proxy's processor time, not a wait.
- Each mover holds a copy of the target's world while it is prepared, which is where the peak of memory comes from: about 3 MB for each of
  50 movers, the size of the world; six seconds after the last move it was 242 MiB, against 235 MiB before.
- The other players' round trips at the 99th percentile stayed under 0.35 ms before and during the moves, at every step. The worst single round trip
  during the 50 moves of the last step was 77,628 µs: a few players waited a long time, which the 99th percentile does not show.
- All the moves were done, and every mover was served by the target afterwards.

### 4. Churn: connections that come and go

| Concurrent connections | Connections made | Failed | Per second | Login p50 | Login p99 | Proxy CPU mean (% of a core) | Memory peak → after (MiB) | Threads peak → after | TIME_WAIT after |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 100 | 3099 | 0 | 880 | 4.1 ms | 20.0 ms | 127 | 82 → 43 | 369 → 12 | 9002 |
| 250 | 3249 | 0 | 2,062 | 8.0 ms | 44.2 ms | 264 | 136 → 48 | 859 → 12 | 6763 |
| 500 | 3499 | 0 | 2,632 | 58.3 ms | 148.7 ms | 398 | 218 → 56 | 1074 → 12 | 7199 |

- Each connection logs in, joins an empty world and leaves 100 ms later. The proxy kept up with every one, and its threads came back to the twelve
  of its start. At 500 at once the proxy used almost four cores, and the median login rose to 58 ms.
- The proxy's memory is not given back after the peak: it stays at 43 to 57 MiB after the step, against 10 MiB at the start. A run that repeats the step
  on one proxy would tell a leak from retained memory; this one does not.
- Each connection leaves about three sockets in TIME_WAIT on loopback, about 9,000 after 3,100 connections. Windows' range of dynamic ports is 16,384, so this step
  is the one that comes closest to the limit of the machine; the harness waits for the count to fall under 3,000 before the next.

### 5. What a byte costs on its own

The cost of the cipher and of compression, measured in one thread over an in-memory pipe, by the code the
proxy uses (`horizon-protocol`'s connection). Chunk-like data compresses 3.2 to 3.7 times here.

| Operation | ns per byte | MB/s on one core |
| --- | --- | --- |
| aes-cfb8 encrypt | 17.2 | 58 |
| aes-cfb8 decrypt | 2.9 | 349 |

| Packet (bytes) | Setting | Send, ns per byte | Receive, ns per byte | Size ratio on the wire |
| --- | --- | --- | --- | --- |
| 2,049 | plain | 0.1 | 0.1 | 1.00 |
| 2,049 | deflate | 37.4 | 2.1 | 3.15 |
| 2,049 | aes | 20.2 | 3.0 | 1.00 |
| 2,049 | deflate+aes | 42.5 | 3.1 | 3.15 |
| 16,847 | plain | 0.1 | 0.1 | 1.00 |
| 16,847 | deflate | 22.4 | 1.4 | 3.62 |
| 16,847 | aes | 17.2 | 3.0 | 1.00 |
| 16,847 | deflate+aes | 27.4 | 2.2 | 3.62 |
| 30,721 | plain | 0.2 | 0.1 | 1.00 |
| 30,721 | deflate | 22.9 | 1.3 | 3.70 |
| 30,721 | aes | 17.2 | 3.0 | 1.00 |
| 30,721 | deflate+aes | 27.7 | 2.1 | 3.70 |

- **Encrypting is the expensive way round**: AES-CFB8 encrypts a byte at a time, about 17 ns a byte or 58 MB/s on one core, and decrypts
  six times faster. A proxy in the online mode encrypts everything it sends to the players. At the 17 MB/s of 500 playing players that is
  about 0.3 core for the cipher alone, on top of what is measured above (this baseline's swarm is offline, so none of it is in the tables).
- Deflating is 23 to 38 ns a byte (26 to 43 MB/s) against 1.4 to 2.5 ns to inflate: for a packet that is compressed, the proxy's work is
  nearly all in compressing it again for the other side.
- Packets under the threshold are not compressed. Those of 100 bytes cost about the same as the large ones when they are.

## What limits first

- **Processor.** At 0.19 to 0.27 % of a core for each playing player, a core holds roughly 400 players, if the cost stays flat, which was not
  measured past 500. The cost of the move, 480 % of a core for 50 at once, and of a burst
  of logins with worlds, is what would be reached first. Compression is the largest part of both.
- **Threads.** 1,512 threads for 500 players; 3,000 would be 9,000. Windows kept them without trouble (stacks are reserved, not used),
  but a thread of its own for each direction is what DESIGN.md section 17.4 says would stop scaling; at these counts nothing showed it.
  The cost of switching between threads is not separated from the rest in these figures.
- **Memory.** Not the first limit: half a megabyte for a player, so 5 GiB for 10,000.
- **Ports.** Each player costs the proxy one ephemeral port, for its connection to the server (the player's own port is on the player's machine; in these runs it is on the
  same machine too). Windows has 16,384 dynamic ports by default (the Linux default is about 28,000), and a port of a connection that closes is kept for about two minutes. The proxy's connections to one server's address and port can therefore
  not pass about 16,000, however many cores it has, without more server addresses or a wider range.

## What the baseline shows about the proxy

Not corrected here, as the task said; for the decision about architecture.

1. **Three threads for each player, not two** (DESIGN.md section 17.4): 1512 at 500 players, 12 + 3N at every step.
2. **A packet is always decompressed and compressed again.** Two settings of compression that look alike cost the same, and the setting of "no compression either side" saves
   a quarter of the processor. A relay that passed a frame as it is when both sides compress at the same threshold would save the compression of the world on each join
   and of every chunk; this was not tried.
3. **A join with a world is the expensive moment of a session**, not what the player does after: 2.7 MB for each of the 500 spent about 1.4 s on the median
   login in a burst.
4. **A move takes the processor of the proxy in proportion to the moves at once**: 0.58 s for 10, 1.9 s for 50. The 0.3 s of prepare that remains for one move is the time the proxy waits
   for the target's world to stop arriving, as the handoff documentation says; whether it could be shorter was not looked at.
5. **The memory of a step is not given back** after churn or a peak (42 to 57 MiB after, 10 MiB before).
6. A warning line appeared once in the 500-player run with the same compression at both sides; the log was not kept, so its cause is not known.

## Limits of these measurements

- **One machine, loopback.** The bots, the synthetic servers and the proxy share the cores and the system's network stack. Real network latency, losses, and the cost of
  the network card's interrupts are absent. Windows loopback is not Linux's: no figure here is a figure for Linux, which the harness compiles for and was not run on.
- **Synthetic servers.** They send the right kinds and sizes of packets, but their chunks are made-up data that compresses like chunks, not world data, and they do no game work.
  A real server would be heavier and the proxy lighter, relatively.
- **The players' side is offline**: no encryption, no session service, no TLS in the management protocol, no PROXY protocol, no lookup cache. Online mode costs what the micro-measures say for the
  cipher, and the session service's latency at login, and neither is in the tables.
- **Short steps**: 30 s at 100, 250 and 500, one run for each (the whole run was made twice). A leak that needs minutes or a slow growth is not seen. The reference in DESIGN.md is "held for several minutes".
- **The ghost** was fed 169 columns and 30 entities for each player and Teleport Entity packets; the other packets it reads (scoreboards, boss bars, tab list, effects) were not sent.
- **The bots' time** is part of what is measured: a bot that is late to read makes the round trip longer. The direct phase has the same bots, which is why the difference is taken.
- **One hang was seen and not explained.** A non-official run of 500 bots, made with an earlier build of the harness, stayed blocked for over an hour after its proxy had gone, and was killed by hand; the same command with the final build ran normally. The cause is not known; the overall time limit (`--max-total-seconds`) exists because of it.
- **The proxy's own processor time on Windows** is an estimate from cycles (see [BENCH.md](BENCH.md)).
- **Not measured:** more than 500 players; a run of minutes or hours; slow clients and half-open connections (DESIGN.md section 17.2's failure row); the login modes other than offline; the balancer,
  the queue and the fallback under load; a proxy under a real player's client (see below).

## Proposed budgets

These are proposals to discuss, fixed from the figures above with a margin; none is a result, and
the numbers hold for this kind of machine and these conditions. The measured value is the one at 500 players.

| Quantity | Measured | Proposed budget |
| --- | --- | --- |
| Resident memory for a connected player (radius 6) | 410 to 640 KiB | at most 800 KiB |
| Threads for a player | 3 | to be decided with the architecture (the budget for 1,000 players on the present design: 3,100) |
| Processor for a playing player, with compression as the server asks | 0.27 % of a core | at most 0.35 % of a core, and flat with the number of players |
| Processor for a player at rest | 0.001 % of a core | at most 0.005 % |
| Added latency, median and 99th percentile | 75 µs and 116 µs | under 150 µs and under 400 µs with 500 playing; DESIGN.md's 1 ms and 5 ms are met by a factor of ten |
| Logins a second with a world of radius 6 | 168 to 307 | at least 150 |
| Logins a second without a world | 3,800 to 4,400 | at least 3,000 |
| Connections that come and go, a second | 880 to 2,632 | at least 800 |
| One move among 10 at once | 0.58 s | under 1 s; 50 at once under 3 s |
| Players dropped or refused | 0 | 0 |

## Running it

```
cargo build --release -p meridian-proxy -p meridian-bench
target/release/meridian-bench load --players 100,250,500 --seconds 30 --out-dir docs/baseline --label "no client"
```

`meridian-bench load --help` lists the options. The scenarios can be chosen (`--scenarios idle,active,moves,churn,micro`), and so can
the size of the world (`--view-radius`, `--entities`), the stream (`--down-rate`, `--chunks`, `--up-rate`), and the settings of compression
(`--compression same,server-off,both-off`). `--append` adds to the CSV files of an earlier pass.

The run needs a free ephemeral-port range: between steps it waits for the sockets of the step before to leave TIME_WAIT (it says so on the standard error).
A smoke test of twenty bots for five seconds, against the synthetic server alone and in front of the proxy when `target/<profile>/meridian` was built,
runs with `cargo test -p meridian-bench`; the move, which takes 30 s, runs with `-- --ignored`.

## Watching a run with a real client

The official figures above were taken **without** a person's client: a graphical client uses the processor and the graphics card of the same machine and would change
the measure. A separate pass lets a person look at the proxy while it is under load, and its figures are labelled
`--label "with a real client"`; they are not to be mixed with the others.

A real client cannot play on a synthetic server, whose packets are scripted and make no valid world. So the proxy routes by the host name the
client types, and the harness adds a **real** server for that host:

- the bots connect to `127.0.0.1`, which is not a forced host: they log in on the default server, a synthetic one;
- the person's client connects to the same proxy as `localhost`, which `[[forced-host]]` leads to the real server (the proxy routes by the host name of the handshake, case-insensitive, with the final dot ignored;
  this is `Config::forced_backend` and was checked by a test, so a route by host is enough). The `127.0.0.1` and `localhost` of the same machine
  reach the same port and are told apart only by the name in the handshake.

[baseline/spectator.example.toml](baseline/spectator.example.toml) shows the configuration the harness then writes. The steps, to be run by a person:

1. Start one real Horizon server on its own port, with the proxy's forwarding secret in its `forwarding.secret` (for the existing bench of
   `banc-lb`, start only its `lobby` server from the panel, and keep that bench's own proxy stopped, since it uses port 25565 too).
2. Start the load, giving the real server's address and the file that holds its secret, so that the proxy and the synthetic servers use it:
   ```
   meridian-bench load --scenarios active --players 500 --seconds 60 --direct-seconds 1 --compression same ^
     --spectator 127.0.0.1:25601 --forwarding-secret-file <the server's forwarding.secret> ^
     --spectator-port 25570 --label "with a real client"
   ```
3. The status line, one a second, shows `S2 proxy` when the proxy is up; then in the client's server list add `localhost:25570` and join (offline account).
   The line reads: `[S2 proxy t=23s] bots 500/500 joined 500 logins/s 0 | proxy rss 233 MiB cpu 131% thr 1509 | rtt p99 0.22 ms | down 17.4 MB/s | free 29.8 GiB`:
   the bots connected, the logins that second, the proxy's memory, processor and threads, the 99th percentile of the round trips of the last second, and what the machine has free.
4. The window lasts 60 s at most. If the real server is not running, the proxy leads the client to the next server, a synthetic one, which a real client cannot play on.

## The 1,000-player step (planned, not run)

Past 500 bots the harness refuses to run unless `--max-bots` is raised, and the person whose machine it is decides when. What to expect at 1,000, from the figures above taken
flat per player (an extrapolation, not a measurement; the moves and the churn are the least certain):

| | Estimate at 1,000 |
| --- | --- |
| Proxy memory | 0.4 to 0.7 GiB (about 0.45 MiB for a player, plus 10 MiB) |
| Memory of the bench itself | about 0.5 GiB (255 MiB at 500 players playing) |
| Threads | about 3,000 in the proxy; about twice that in the bench (one for each bot, one to read, one to act, one for the server's side) |
| Processor, active | the proxy about 2.7 cores, the bench about 3.4 cores |
| Processor, moves and churn | the proxy up to about 10 cores for some seconds, with 100 moves at once or 1,000 connections that come and go |
| Ports | about 2,000 in use at once (one for each bot, one for each connection of the proxy to a server), and about 9,000 more in TIME_WAIT at the end of the churn |
| Duration | about 15 minutes, plus the waits between steps for TIME_WAIT to fall under 3,000 |

The dynamic range of Windows (16,384 ports) holds this on loopback, so one address is enough and none of the `127.x.x.x` addresses is needed; the harness waits between the steps.
A run that wanted 3,000 or more players on one machine would run out of ports in the churn and would have to give the servers and the proxy more addresses, or widen the range.

The harness stops a run on its own when the memory free falls under `--min-free-gib` (give 8 for this step), when the proxy is gone, when more than a tenth of the bots are lost or
the reading of the proxy is starved for more than four seconds. A step that has not finished after `--max-total-seconds` is stopped, and cut a minute later (the proxy is killed and the program leaves).

```
cargo build --release -p meridian-proxy -p meridian-bench -j 3
meridian-bench load --players 1000 --max-bots 1000 --seconds 30 --direct-seconds 20 --min-free-gib 8 ^
  --max-total-seconds 2400 --scenarios idle,active,moves,churn --out-dir docs/baseline/n1000 --label "no client"
```

The files go to their own folder because they have columns that the older files do not.
