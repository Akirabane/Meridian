**One night, one morning and one afternoon of measurements on one Windows machine, over the loopback, with real Horizon
servers. They say what this proxy cost there, on 8 and 9 November 2026. They are not its performance in production, they
are not a promise, and several of the runs came out with no result: this page lists them too.**

# The proxy in front of real Horizon servers: the level-2 campaign

[LOAD-1000.md](LOAD-1000.md) measured the proxy against synthetic servers. This page replaces the synthetic servers
by real `horizon-server` processes, with their engine, each holding its own world. It gives:
- what held, with the rule that was fixed before each run, the number of runs and the load of the machine;
- the compression's share of the proxy's processor time, and the effect of the relay of the network simulator;
- the comparisons of the compression level, and which of them could be read;
- what is **not** a result: runs that were invalid, a size that was not run, and comparisons that could not be judged.

## In short

| Question | Answer |
| --- | --- |
| 100 players on one real server | **Holds.** 0.44 cores of the proxy in each of nine steps (0.435 to 0.444; seven of them "not clean", section 2), about 190 to 200 MB |
| 125 players on one real server | **Holds, one run:** 0.58 cores, 250 MB; the server's tick p95 was 37.9 ms |
| 250 players on 3 real servers | **Holds, one run:** 1.24 cores, 456 MB |
| 500 players on 6 real servers | **Holds, one run:** 3.31 cores, 875 MB |
| 750 players on 9 real servers | **No result**, twice: the servers could not carry the load on this PC (disconnections in the first run, five servers under the rule in the second). Nothing is concluded about the proxy |
| 1,000 players on real servers | **Not run** (the plan stops where 750 stops) |
| Share of the compression in the proxy's processor time | **About 41 %** (towards the players); the part towards the servers, about 4 %, **cannot be told from zero** |
| A relay that delays each way by 20 ms | A round trip grew by **40.05 ms** more than the proxy's own 49 µs, for two delays of 20 ms (one run, with 1.39 cores of other load: not clean) |
| Compression level 4 against 6 | **Shown** at 100 players (see [OPERATING.md](OPERATING.md), section 5.14) |
| Compression level 3 against 6, and a level 4 against 6 pair at 500 players | **Not conclusive** and **not judgeable**: section 5 |
| First view of a player, one bot | **+0.35 s** (level 6), **+0.25 s** (4), **+0.10 s** (1) over 1.04 s: section 8 |

## 1. The machine and how a step was run

| | |
| --- | --- |
| Machine | Windows 11, Intel Core i9-14900KF: 8 performance cores with two threads each (logical 0-15) and 16 efficiency cores (logical 16-31); 64 GB. Servers, proxy, bots and samplers all on it, over `127.0.0.1` |
| Servers | real `horizon-server` and its engine, from one Horizon commit (`1597853de7`), release build; one process each, started from a copy of the same template world (about 22,800 chunks generated around the spawn beforehand), view and simulation distance 10, creative, offline mode, at a low priority |
| Players | `horizon-bots`, behaviour `mixed`: bots that walk, that explore at 0.8 blocks a second, and that stand still. They are creative-mode flyers; they do not mine, build or fight. Joins every 0.2 to 0.5 s |
| Proxy | Meridian `b734ba9` (main after #160) for the steps of sections 2 to 4; `1a6fe90` (main `337d0ff` plus the `compression-level` setting) for the comparisons of section 5; `337d0ff` for the compressor pool's comparison (section 6); release builds, balancing on, soft cap 85 and hard cap 110 for steps of several servers (200 for one server), compression threshold 256 on both sides |
| Placement | the servers, the proxy, the bots and the samplers each pinned to their own logical processors. The proxy ran on 8-11 (four threads of performance cores) at 100, 125 and 250 players, and on 16-19 (four efficiency cores) at 500 |
| A step | 5 minutes of warm-up, then 10 minutes measured, one step after another, from a fresh set of servers |
| Sampling | once a second: each server's TPS, tick time and chunks (its own metrics page), the proxy's processor (by the processor cycles Windows counts, the figure used here; the time-based figure is noisier), memory and threads, and the load of every logical processor |

**The rule of validity**, fixed before the runs. A step counts only if every server's mean TPS is at least 19.5, the
mean of each server's tick-time p95 is under 50 ms, and no bot is lost. A step that fails it is a result about the
machine or the servers, and is written as such: nothing is read from it about the proxy.

**The load of the machine.** Before each step a check read the load of every other process (any program that is not the
step's) and refused to start while something was being built. The steps of sections 2 and 3 ran with 0.27 to 0.35 cores
of other load on average in the window (at most 1.43 cores in a 5-second sample), and the machine was 0 to 6 % busy
before each of them, 14 % before the 500-player step. The step of section 4 with the relay had 1.39 cores of other load,
and section 6 lists the runs for which the load was higher.

**The raw data** (a CSV per step, the samplers' files and the analysis) is kept on the machine that ran it. It is not in
this repository.

## 2. What held

One run of each size, except where the table says otherwise. The "cores" are the proxy's processor time in a window, in
cores of this machine.

| Step | Proxy cores | % of a core a player | Proxy memory | Servers' mean TPS | Tick p95, mean of each server | Other load, mean (max) |
| --- | --- | --- | --- | --- | --- | --- |
| 100 players, 1 server (two runs) | 0.444 and 0.441 | 0.444 and 0.441 | 200 and 192 MB | 20.00 | 32.0 ms | 0.31 and 0.31 (0.70, 0.76) |
| 125 players, 1 server | 0.58 | 0.463 | 250 MB | 20.00 (lowest second 19.20) | 37.9 ms | 0.29 (0.56) |
| 250 players, 3 servers (85, 85, 80) | 1.24 | 0.495 | 456 MB | 20.00 (lowest second 19.60) | 31.2 to 35.9 ms | 0.35 (1.06) |
| 500 players, 6 servers (85 each, 75 on the last) | 3.31 | 0.663 | 875 MB | 20.00 (lowest second 19.15) | 35.3 to 42.2 ms | 0.30 (0.98) |

No bot was lost in any of these steps and every one passed the rule above.

**Nine steps at 100 players on one server** with the default compression level (the two above and seven more taken from
other comparisons of the campaign, on three builds of the proxy) used 0.435 to 0.444 cores. Those seven had 0.72 to 0.96
cores of other load in their windows (not clean, over 0.5), so the spread between runs, about 2 %, is said with that
reserve.

**What the table shows:**
- the cost of a player is not flat in these steps: 0.44, 0.46, 0.50 and 0.66 % of a core at 100, 125, 250 and 500
  players. The reason is not known. The 500 step also ran the proxy on efficiency cores, which are slower than the
  performance cores of the other steps, so its figure is not on the same footing as the others;
- memory is 1.8 to 2.1 MB a player at every size;
- these figures are for these bots on this machine. They do not give a figure for 750 or 1,000 players: the cost per
  player rose between sizes, and a straight line would be a guess.

### One server on its own

One real server, in the afternoon of 8 November, one run each, valid by the rule, through the proxy:

| Players | 50 | 75 | 100 | 125 (afternoon) | 125 (night, calm) |
| --- | --- | --- | --- | --- | --- |
| Tick, mean over the window | 17.1 ms | 20.6 ms | 29.4 ms | 41.1 ms | 33.9 ms |
| Other load, mean | | | | 1.64 cores | 0.29 cores |

The afternoon's 125 players ran while the machine was used by other programs (1.6 cores of other load on average, over 1
core for 456 of 569 s), and its tick was 41 ms. The same size repeated at night with 0.29 cores of other load gave 34
ms. So the edge of one server is not read from the afternoon run: it depends on the background load. The step with 50
players was also run direct, with no proxy: the tick was 16.9 ms direct and 17.1 ms through the proxy, with the same
chunks and about the same bytes received (one run of each).

## 3. What the compression costs

**How it was run.** 100 players on one real server, four steps in this order, each with a different compression:

| Step | Towards the servers | Towards the players | Proxy cores |
| --- | --- | --- | --- |
| `same` | compressed | compressed | 0.444 |
| `server-off` | not compressed | compressed | 0.424 |
| `both-off` | not compressed | not compressed | 0.241 |
| `same` again | compressed | compressed | 0.441 |

The share of the proxy's processor time is the difference between steps divided by the mean of the two `same` steps
(0.4425):
- **towards the players:** (0.424 − 0.241) / 0.4425 = **41 %**;
- **towards the servers:** (0.4425 − 0.424) / 0.4425 = about **4 %**. One run of each profile and a spread of about 2 %
  between runs: 4 % **cannot be told from 0**;
- **both together:** 46 %.

The servers kept 20.00 TPS in all four. The bandwidth was not read here. The two `same` steps (0.444, 0.441) are within
2.3 % of the earlier run of the same size (0.434), which is as close as the 3 % the plan asked for.

The 41 % is for bots that explore a world, which load chunks all the time; the chunks are the large packets that
compress. A real player who moves less loads fewer chunks, so the share is probably smaller.

## 4. The relay of the network simulator

The simulator (`meridian-bench netsim`, [NETSIM.md](NETSIM.md)) sat in front of the proxy with `--delay 20`: 20 ms each
way. Twenty echo probes went through the proxy, against simulated echo backends used only for the probes' own traffic;
the 100 players were on a real server. Two steps, one run each:

| | Without the relay | With the relay |
| --- | --- | --- |
| Probe round trip direct (no proxy) | 65 µs | |
| Probe round trip through the proxy | 114 µs | 40,167 µs |
| Added by the proxy (through minus direct) | 49 µs (p99 878 µs) | 40,102 µs (p99 42,105 µs) |

With the relay the added time grew by **40.05 ms** (40,102 µs against 49 µs) for a delay of 20 ms each way, that is
two delays of 20,000 µs and about 50 µs more. The relay carried 170 connections and held a chunk 20.1 ms on average on
the way down and 20.3 ms on the way up, writing 85 µs and 323 µs late on average; no connection reset, no stall,
and it stopped cleanly. The servers kept 20.00 TPS, and the 100 players cost the proxy 0.45 cores in both steps.

The step with the relay is not clean: other programs used 1.39 cores on average in its window (0.35 in the step without
it). How much that load changed the figures was not measured: read the step as run with more load than the others.

This says that the relay and the proxy forward as measured. It is not a conclusion about real servers or a real network
(see the limits, section 7).

## 5. The compression-level comparisons

Each comparison runs the same proxy binary at two levels, in the order A, B, A, B, with 100 players on one real server,
and the rules **fixed before the runs**: both B runs below both A runs; the gap at least twice the larger spread inside
one side; at least 3 % of A's processor time; B's bytes received at most 5 % above A's. A comparison is "conclusive"
only if, besides, the other load of the machine was the same in all the steps within 0.15 cores. A step with more than
0.5 cores of other load is "not clean": it is reported. The pair at 500 players on 6 servers is one run of each side
with a lower bar, set before it: "consistent" if B is at least 3 % lower with at most 5 % more bytes, and never "shown"
on one run.

| Comparison | Result |
| --- | --- |
| Level 4 against 6, 100 players, 1 server, four steps | **Shown.** The four rules held. See [OPERATING.md](OPERATING.md), section 5.14, for the figures. The other load was 0.66 to 0.73 cores in every step: "not clean", but the same on both sides within 0.07 |
| Level 3 against 6, 100 players, 1 server, four steps | **Not conclusive: background load.** All four steps were valid and the four rules held, but the other load of the four steps (0.72 to 0.96 cores, and falling during the morning) differed by 0.24 cores, over the 0.15 allowed. **No figure is claimed for level 3** |
| Level 4 against 6, 500 players on 6 servers, **one run of each** | **Not judgeable.** Both steps were valid. A program of the machine used about 3 cores in both windows (more in the first) and the servers sent a little more work in the second, so the two sides were not under the same conditions. One run each gives no spread to read an effect against. **No figure is claimed at 500 players** |

Level 3's comparison can be redone with a quieter machine. Neither non-result says level 3 or the 500-player pair has no
effect: they say it was not shown on this machine.

## 6. What is not a result

- **750 players on 9 servers: no result, twice.**
  - First run, on the night of 8 to 9 November: all 750 bots joined, but three minutes in, while the last ones were
    still joining, the servers began to disconnect players with `disconnect.timeout`, the proxy moved them to other
    servers, which then did the same, and 220 players were left. The load of each processor showed afterwards that the
    servers' work sat on four efficiency cores that their mask allowed, while the performance-core threads were nearly
    idle (probably because the servers run at a low priority; not proven).
  - Second run, the servers on the performance cores only (`FFFF`), the proxy on 6 efficiency cores: all 750 bots joined
    and none was lost, but five of the nine servers missed the rule (mean TPS down to 18.59, or a tick p95 of 51 to 58
    ms), with 10.3 cores for the servers and a run queue of 65. The limit was **the machine's performance cores**, not
    the proxy: nothing can be concluded about the proxy at 750.
  - One observation is kept and not concluded from: in that run the proxy used 6.27 cores on its six pinned processors
    (96 to 97 % busy), 0.84 % of a core a player. Either its cost per player grows with the number of sessions, or it
    was held back by its six processors. A run with the proxy on more processors, at a size the servers can hold,
    would tell.
- **1,000 players on real servers: not run.** The plan stops where 750 stops. The next size it names is 625 players on
  8 servers; it was not started.
- **The first 250-player run** (8 November, afternoon) has no server data because of a fault in the sampler script for
  several servers, and was redone after the fix; only the redone step is above.
- **The first 125-player run** (8 November, afternoon) ran with 1.64 cores of other load: it is valid by the rule but
  not clean, and the clean repeat is the one in the table.
- **A pool of compressors: not shown.** A candidate change to the proxy's compression (a pool of compressors) was run
  against the build without it at 100 players on one server, in the order A, B, A, B (the first A redone, for its
  other load). It lowered the proxy's processor time by about 1.7 % (0.0075 cores, 0.4285 against 0.436), under the 3 %
  fixed beforehand, so the effect is **not shown**, and no gain on real servers is claimed for the change. B was not
  worse than A, and the work was the same on both sides. Every step had 0.66 to 0.79 cores of other load (not clean).
- **Level 3 and the 500-player pair:** section 5.

## 7. What these figures do not say

- **Real players.** The bots are creative flyers that explore; they do not mine, build, fight or trade. A real server's
  tick under such players is not measured.
- **A real network.** Everything is on the loopback; the relay of section 4 adds a delay, not losses. One run with real
  servers over a real network is in [LOAD-1000.md](LOAD-1000.md), section 5.
- **The online mode.** The bots are offline: no encryption, no account check.
- **Linux.** Every figure here is a Windows figure.
- **More than 500 players.** Not measured: see section 6.
- **A quiet machine.** The valid steps had 0.27 to 0.35 cores of other load; the machine was not otherwise empty.
- **One run each**, except where a row says otherwise: the spread between runs is known only at 100 players (about 2 %).
- **Processor figures.** They are counted from the processor cycles Windows gives the process, on cores of two kinds;
  the 500 step's proxy ran on efficiency cores, so its figure cannot be set beside the others' without that caveat.

## 8. The first view of a player (473 chunks, one bot that follows vanilla's ACK formula)

The other sections load a server with many players. This one measures the opposite: how long **one** player waits for
the world around them to arrive after joining, and what the proxy adds to it. It was run on the afternoon of 9 November
2026, on the same machine, with the same Horizon build as section 1 and the Meridian build of section 5. Local runs, one
bot, three passes per scenario: a first reading of the order of the effect, not a statistic.

**How it was run.** One `horizon-bots` bot, behaviour `idle`, joined a real `horizon-server` whose world is the template
of section 1 (view distance 10): the first view is **473 chunk packets**, about 22.8 MB once decompressed (the same in
every run, to within 0.3 %). After each batch of chunks the bot answers with the number of chunks per tick that
vanilla's formula gives, computed from how long the batch took to arrive: the server sizes its next batch from that
answer, as it does for a vanilla client. What is read is the time, in milliseconds from the first batch's start, of each
chunk packet at the bot. The figure used is the time of the **329th** chunk, the point at which the issue Horizon #9052
quotes about 1.0 s on a direct connection; the time of the last (473rd) chunk is given next to it. Two servers stayed up
for the whole campaign (one reached through the proxy with a forwarding secret, one direct), each warmed by one join,
and the order of the scenarios was shuffled in each round. The servers, the proxy, the relay and the bot were each
pinned to their own processors, at a low priority. Three passes per scenario, two for the scenarios with a delay and no
rate: 40 runs, and eight more (two per path) for the bytes.

### The proxy alone: no delay, no rate limit

| Path | 329th chunk | Last chunk | Against direct, 329th | Bytes on the link, against direct |
| --- | --- | --- | --- | --- |
| Direct (no proxy) | 1.04 s | 1.34 s | | 4.53 and 4.46 MB |
| Proxy, level 6 | 1.39 s | 1.89 s | **+0.35 s (+34 %)** | 1.0 % and 1.3 % **less** |
| Proxy, level 4 | 1.29 s | 1.69 s | **+0.25 s (+24 %)** | 0.5 % and 1.2 % more |
| Proxy, level 1 | 1.14 s | 1.48 s | **+0.10 s (+9 %)** | 32 % and 35 % more |

The times are the median of three passes; the three passes of one path differ by 0.1 to 1.5 % (1.04 s: 1.039 to 1.040;
level 6: 1.387 to 1.408). The bytes are the proxy's relay's own count of what reached the bot, two passes per path (the
"bytes" runs, not the same passes as the times). **Levels 6 and 4 send about what a direct connection does (between
1.3 % less and 1.2 % more), and level 1 a third more.** [OPERATING.md](OPERATING.md), section 5.14, gives the same order
for the compression alone (level 4 about 3 % more than level 6, level 1 about 38 % more).

A fixed delay alone changes little: 25 ms each way (a 50 ms round trip), direct, gives 1.16 s for the 329th chunk
(+0.12 s over 1.04 s, two passes) with batches of the same size (a median of 17 chunks against 18), and nothing
collapses.
Through the proxy at level 6 with the same delay it is 1.46 s (two passes), +0.30 s over the direct run with that delay.

### Behind a link that is limited

A relay in front of the bot (section 4) held the link to a rate and a delay of 25 ms each way. Direct, and through the
proxy at level 6, three passes each (median):

| Rate | Direct: 329th / last | Proxy 6: 329th / last | Added, 329th | Added, last | Median batch, direct / proxy |
| --- | --- | --- | --- | --- | --- |
| 100 Mbit/s | 1.79 s / 2.49 s | 1.93 s / 2.64 s | +8 % | +6 % | 10 / 9 |
| 50 Mbit/s | 2.78 s / 3.98 s | 3.19 s / 4.39 s | +15 % | +10 % | 6 / 5 |
| 20 Mbit/s | 4.98 s / 6.89 s | 5.73 s / 8.13 s | +15 % | +18 % | 3 / 3 |
| 10 Mbit/s | 6.48 s / 9.08 s | 8.23 s / 11.83 s | +27 % | +30 % | 3 / 2 |

The batches shrink as the rate falls, with or without the proxy. At 10 Mbit/s the first batches are 9, 1, 1, 2, 2, 3, 3,
3 chunks direct and 9, 1, 1, 2, 2, 2, 2, 2 through the proxy; with no limit, direct, they are 9, 5, 6, 8, 9, 10, 11, 12.
No run reached a median batch of one chunk: the smallest medians are 3 direct and 2 through the proxy, and every first
view ended.

### What is measured, and what is deduced

**Measured.** The times, the batch sizes and the bytes above; that the proxy makes the first view later at every level
and every rate that was run; that a delay alone does not; that the batches shrink behind a limit with or without the
proxy (a little more with it).

**Deduced, not measured.**
- *Where the proxy's time goes.* The proxy decompresses each chunk packet from the server and compresses it again
  towards the player. The extra 0.35 s over 329 chunks is about 1.1 ms a chunk at level 6 (0.75 ms at level 4, 0.30 ms
  at level 1), which is the order of a recompression. That time is added to what the client measures, so the client asks
  for fewer chunks. This is a reading of the figures, not a profile.
- *Why the batches shrink.* The wire carries only 4.5 MB, about a fifth of the 22.8 MB: at 10 Mbit/s the link alone
  would take 3.6 s, and the whole view took 9.1 s direct (2.5 times that); at 100 Mbit/s it would take 0.36 s and took
  2.5 s (6.9 times). The speed is not set by the bandwidth but by the client's own estimate of it, which asks for fewer
  chunks when a batch arrives slowly. That is vanilla's design, the server reproduces it, and it is the same behind any
  hop: the proxy creates none of it and adds a little to it.
- *Not explained.* At 10 Mbit/s the proxy adds +27 % (1.75 s), more than a flat 1 ms a chunk would give (329 ms, 5 % of
  the direct 6.48 s). The recompression alone does not account for it, and it was not looked into.

### What this does not say

- **A real kernel TCP path.** The relay runs on the loopback: it models a delay and a rate, not a round trip through a
  kernel's TCP with a slow start, nor losses. The collapse of the batches to one chunk that Horizon #9052 describes,
  over a long TCP tunnel, was **not reproduced**: no median batch of one chunk appeared in the 40 runs.
- **A real vanilla client.** The bot is a Horizon test client that applies vanilla's formula. It dates the batch markers
  in its reading thread, where a real client does it on its main thread, once a frame; probably the bot shows a lower
  bound of the effect (a reading, not measured).
- **More than one player.** One bot, one world, one machine, Windows.
- **How many passes.** Three (two for the scenarios with a delay and no rate): the passes of one scenario differ by
  2.2 % or less in 11 of the 14 scenarios, and by 3.1 % and 4.5 % (two passes each) and 8.4 % in the other three. The
  8.4 % is the direct run at 100 Mbit/s, one pass of which took 1.94 s against 1.79 s for the other two, in runs whose
  other load was 0.34 to 0.44 cores (clean, see the next point); it was not explained.
- **A quiet machine.** Six of the 40 runs are "not clean" (other programs used more than 0.5 cores on average during the
  run): 0.52 to 0.65 cores (the other 34 runs: 0.30 to 0.49), at most 1.26 cores in a 3-second sample. No visible effect
  on the figures was found, but it was not measured.
- **The default.** These figures do not change [OPERATING.md](OPERATING.md)'s advice on the compression level: they are
  a second thing to weigh for level 4, next to the processor time.
