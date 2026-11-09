**One night and one morning of measurements on one Windows machine, over the loopback, with real Horizon servers. They say what this proxy cost there, on 8 and 9 November 2026. They are not its performance in production, they are not a promise, and several of the runs came out with no result: this page lists them too.**

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
| 100 players on one real server | **Holds.** 0.44 cores of the proxy in each of nine steps (0.435 to 0.444), about 190 to 200 MB |
| 125 players on one real server | **Holds, one run:** 0.58 cores, 250 MB; the server's tick p95 was 37.9 ms |
| 250 players on 3 real servers | **Holds, one run:** 1.24 cores, 456 MB |
| 500 players on 6 real servers | **Holds, one run:** 3.31 cores, 875 MB |
| 750 players on 9 real servers | **No result**, twice: the servers could not keep 20 ticks a second on this PC. Nothing is concluded about the proxy |
| 1,000 players on real servers | **Not run** (the plan stops where 750 stops) |
| Share of the compression in the proxy's processor time | **About 41 %** (towards the players); the part towards the servers, about 4 %, **cannot be told from zero** |
| A relay that delays each way by 20 ms | A round trip grew by **40.05 ms** more than the proxy's own 49 µs, for two delays of 20 ms (one run, with 1.39 cores of other load: not clean) |
| Compression level 4 against 6 | **Shown** at 100 players (see [OPERATING.md](OPERATING.md), section 5.14) |
| Compression level 3 against 6, and a level 4 against 6 pair at 500 players | **Not conclusive** and **not judgeable**: section 5 |

## 1. The machine and how a step was run

| | |
| --- | --- |
| Machine | Windows 11, Intel Core i9-14900KF: 8 performance cores with two threads each (logical 0-15) and 16 efficiency cores (logical 16-31); 64 GB. Servers, proxy, bots and samplers all on it, over `127.0.0.1` |
| Servers | real `horizon-server` and its engine, from one Horizon commit (`1597853de7`), release build; one process each, started from a copy of the same template world (about 22,800 chunks generated around the spawn beforehand), view and simulation distance 10, creative, offline mode, at a low priority |
| Players | `horizon-bots`, behaviour `mixed`: bots that walk, that explore at 0.8 blocks a second, and that stand still. They are creative-mode flyers; they do not mine, build or fight. Joins every 0.2 to 0.5 s |
| Proxy | Meridian `b734ba9` (main after #160), release build, balancing on, soft cap 85 and hard cap 110 for steps of several servers (200 for one server), compression threshold 256 on both sides |
| Placement | the servers, the proxy, the bots and the samplers each pinned to their own logical processors. The proxy ran on 8-11 (four threads of performance cores) at 100, 125 and 250 players, and on 16-19 (four efficiency cores) at 500 |
| A step | 5 minutes of warm-up, then 10 minutes measured, one step after another, from a fresh set of servers |
| Sampling | once a second: each server's TPS, tick time and chunks (its own metrics page), the proxy's processor (by the processor cycles Windows counts, the figure used here; the time-based figure is noisier), memory and threads, and the load of every logical processor |

**The rule of validity**, fixed before the runs. A step counts only if every server's mean TPS is at least 19.5, the mean
of each server's tick-time p95 is under 50 ms, and no bot is lost. A step that fails it is a result about the machine
or the servers, and is written as such: nothing is read from it about the proxy.

**The load of the machine.** Before each step a check read the load of every other process (any program that is not
the step's) and refused to start while something was being built. The steps of sections 2 and 3 ran with 0.27 to 0.35
cores of other load on average in the window (at most 1.43 cores in one second), and the machine was 0 to 6 % busy
before each of them, 14 % before the 500-player step. The step of section 4 with the relay had 1.39 cores of other
load, and section 6 lists the runs for which the load was higher.

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

**Nine steps at 100 players on one server** with the default compression level (the two above and seven more taken from the
comparisons of section 5, with three builds of the proxy) used 0.435 to 0.444 cores: the spread between runs is about 2 %.

**What the table shows:**
- the cost of a player is not flat in these steps: 0.44, 0.46, 0.50 and 0.66 % of a core at 100, 125, 250 and 500 players.
  The reason is not known. The 500 step also ran the proxy on efficiency cores, which are slower than the performance
  cores of the other steps, so its figure is not on the same footing as the others;
- memory is 1.8 to 2.1 MB a player at every size;
- these figures are for these bots on this machine. They do not give a figure for 750 or 1,000 players: the cost per player
  rose between sizes, and a straight line would be a guess.

### One server on its own

One real server, in the afternoon of 8 November, one run each, valid by the rule, through the proxy:

| Players | 50 | 75 | 100 | 125 (afternoon) | 125 (night, calm) |
| --- | --- | --- | --- | --- | --- |
| Tick, mean over the window | 17.1 ms | 20.6 ms | 29.4 ms | 41.1 ms | 33.9 ms |
| Other load, mean | | | | 1.64 cores | 0.29 cores |

The afternoon's 125 players ran while the machine was used by other programs (1.6 cores of other load on average, over
1 core for 456 of 569 s), and its tick was 41 ms. The same size repeated at night with 0.29 cores of other load gave 34 ms.
So the edge of one server is not read from the afternoon run: it depends on the background load. The step with 50
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
  - Second run, the servers on the performance cores only (`FFFF`), the proxy on 6 efficiency cores: all 750 bots
    joined and none was lost, but five of the nine servers missed the rule (mean TPS down to 18.59, or a tick p95 of 51 to
    58 ms), with 10.3 cores for the servers and a run queue of 65. The limit was **the machine's performance cores**, not
    the proxy: nothing can be concluded about the proxy at 750.
  - One observation is kept and not concluded from: in that run the proxy used 6.27 cores on its six pinned processors
    (96 to 97 % busy), 0.84 % of a core a player. Either its cost per player grows with the number of sessions, or it
    was held back by its six processors. A run with the proxy on more processors, at a size the servers can hold,
    would tell.
- **1,000 players on real servers: not run.** The plan stops where 750 stops. The next size it names is 625 players on
  8 servers; it was not started.
- **The first 250-player run** (8 November, afternoon) has no server data because of a fault in the sampler script for
  several servers, and was redone after the fix; only the redone step is above.
- **The first 125-player run** (8 November, afternoon) ran with 1.64 cores of other load: it is valid by the rule but not
  clean, and the clean repeat is the one in the table.
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
