**One run, on a home network, against synthetic servers, over a link of 100 Mbit/s that the worlds
filled. It says what 250 bots saw of one proxy on a small laptop on 5 November 2026, and what that
machine read of the proxy. It is not the proxy's limit, and it was not repeated.**

# 250 bots on a proxy on another machine

This is the first run of the load against a proxy that is not on the machine of the bots
([LOAD-REMOTE.md](LOAD-REMOTE.md) describes the mode). The bots arrived in two waves of 125, thirty seconds
apart. What the bots saw is below, then what the proxy's machine read of its own process, then what the run does
not say.

## The machines and the run

| | |
| --- | --- |
| Machine A (bots, synthetic server) | Windows 11, Intel Core i9-14900KF, 32 logical cores, about 31 GiB of memory free during the run, address 192.168.1.18 |
| Machine B (the proxy) | An ASUS VivoBook X421DA laptop, 192.168.1.25: AMD Ryzen 5 3500U (4 cores, 8 threads, 1.4 to 2.1 GHz, about 2.7 GHz on average at the boost seen), 5,877 MiB of memory (5.9 GB) and 6 GB of swap, NVMe disk, Debian 13 (trixie), kernel 6.12.115, power saving off. The proxy runs as a systemd service limited to 600 % of processor, built from commit `8e403b52` of `main` (after #94) |
| Link | Ethernet through a **USB Realtek RTL8153 adapter that negotiated 100 Mbit/s full duplex**, though it can do 1 Gbit/s (the cable or the port may be the cause; not looked at). The laptop's Wi-Fi was on |
| Date | 5 November 2026; the bench started at 18:07:11 UTC |
| The bench | `meridian-bench` of commit `cbd52e4` (release build), from `D:\disque\dev\meridian-bench-out\bin-lan\` |

The proxy read a configuration that the bench wrote for it: offline mode, no limit on players or on connections,
a compression threshold of 256 on both sides, one backend `lobby` at `192.168.1.18:25701` (the synthetic server,
on machine A), a secret made for this run, and the metrics page on its loopback.

```
meridian-bench load --proxy 192.168.1.25:25565 --backend-bind 192.168.1.18 --backend-ports 25701-25710 ^
  --write-remote-config D:\disque\dev\meridian-bench-out\banc-lan\debian-load --meridian <meridian program>
```

The firewall of machine A let the proxy in on the ports of the synthetic servers for the bench's program only (the rule is in
[LOAD-REMOTE.md](LOAD-REMOTE.md)). A first run of ten bots checked the whole path; then the run itself:

```
meridian-bench load --proxy 192.168.1.25:25565 --backend-bind 192.168.1.18 --backend-ports 25701-25710 ^
  --remote-config D:\disque\dev\meridian-bench-out\banc-lan\debian-load\meridian.toml ^
  --players 250 --waves 2 --wave-gap-seconds 30 --seconds 30 --direct-seconds 20 ^
  --max-total-seconds 900 --max-lost-percent 100 --min-free-gib 12 ^
  --out-dir D:\disque\dev\meridian-bench-out\banc-lan\run-250 --label "lan 250 2 waves"
```

The bots are those of [BASELINE.md](BASELINE.md): each one is sent a world of 169 chunk columns and 30 entities (about
2.7 MB before compression), then 20 small packets and 2 chunk packets a second, and sends 20 movement packets and 2 measuring
packets a second. The run has two steps, `idle` (the bots hold the connection for 30 s) and `active` (a phase straight to
the synthetic server on machine A, 20 s, then the same bots through the proxy, 30 s measured). In each, 125 bots log in at once, and
125 more thirty seconds after the last of the first 125 is in. `--max-lost-percent 100` let the run go on if bots had been lost;
none were.

## What the bots saw

The times are UTC, by the clock of machine A.

### The phases

| Step | Phase | Start | End | Length |
| --- | --- | --- | --- | --- |
| idle | join, wave 1 | 18:07:14.453 | 18:07:25.797 | 11.3 s |
| idle | join, wave 2 | 18:07:55.808 | 18:08:06.857 | 11.0 s |
| idle | hold | 18:08:09.878 | 18:08:39.881 | 30 s |
| active, direct | join | 18:08:44.994 | 18:08:46.515 | 1.5 s |
| active, direct | measure | 18:08:51.565 | 18:09:11.566 | 20 s |
| active, through the proxy | join, wave 1 | 18:09:11.636 | 18:09:24.267 | 12.6 s |
| active, through the proxy | join, wave 2 | 18:09:54.290 | 18:10:09.211 | 14.9 s |
| active, through the proxy | measure | 18:10:14.261 | 18:10:44.309 | 30 s |

A join is the time from the first bot of a wave starting to the last one having its world. The whole run took about three and a half minutes.

### Bots in, and bots lost

All 250 bots logged in and got their world in each step: 125 of 125 in each wave of `idle` and of `active`, none failed to log in, none was cut off.
The 916 keep-alives the server sent in `idle` were all answered. The guards of the bench (free memory under 12 GiB, more than a tenth of the bots lost, a run too long) did not trip.

### Login times

The time to log in is from the bot connecting to the end of the configuration phase. It **does not include the world**, which is what takes the
11 to 15 seconds above.

| Step, wave | p50 | p95 | p99 | max |
| --- | --- | --- | --- | --- |
| idle, wave 1 | 97.6 ms | 1,813.7 ms | 1,815.5 ms | 1,815.7 ms |
| idle, wave 2 | 52.7 ms | 1,289.5 ms | 1,292.8 ms | 1,293.2 ms |
| active, wave 1 | 37.1 ms | 1,576.5 ms | 1,586.6 ms | 1,652.1 ms |
| active, wave 2 | 296.3 ms | 756.3 ms | 758.2 ms | 758.5 ms |

The 95th percentile, the 99th and the maximum are close: a few bots in each wave waited between 0.8 and 1.8 s while the others did not.

### The bots already in, while the second wave came

The `active` bots of wave 1 sent two measuring packets a second all through. Their round trips at the 99th percentile, through the proxy:

| Window | Round trips at the 99th percentile |
| --- | --- |
| The 10 s before wave 2 began | 2.03 ms |
| While wave 2 joined (14.9 s, 938 round trips) | **480.9 ms** (the worst 492.9 ms) |
| The 10 s after wave 2 was in | 2.74 ms |

For the fifteen seconds in which 125 worlds crossed, the players already in had a latency of about half a second, and it was back to a
few milliseconds as soon as the worlds were in. The status line of the bench shows the same second by second: the 99th percentile of the
round trips of the last second was between 487 and 493 ms at its worst. The join of wave 1 (18:09:11 to 18:09:24) crossed the same link, but no bot was in yet to be measured.

### Steady state

With the 250 bots in, over the 30 s window (15,027 round trips):

| | Direct (machine A only) | Through the proxy | Added |
| --- | --- | --- | --- |
| p50 | 68 µs | 819 µs | +751 µs |
| p95 | 91 µs | 1,846 µs | +1,755 µs |
| p99 | 115 µs | 2,833 µs | +2,718 µs |
| max | | 46,040 µs | |

**What the proxy "adds" here includes the network**: the packet goes from machine A to B, from B back to the synthetic server on A, and back
the same way, and the two links are the 100 Mbit/s one. The figure is not the proxy's own cost, and the proxy's cost cannot be taken from it.

### Throughput

The bots count the bytes of the packets they receive once decompressed: 8.70 MB/s (6,017 packets a second down, 5,500 up) in the measured window, about 70 Mbit/s
before compression and about 25 Mbit/s on the wire, since the chunks compress about three to one. During a join, the bots counted up to
55.5 MB/s in a second. A wave is 125 × 2.7 MB, about 337 MB, which crossed in 11 to 15 s: about 23 to 30 MB/s counted, 65 Mbit/s
on the wire on average, in a link that carries at most 100.

### The network between the machines

The bench opens 50 connections to the proxy, one at a time, and notes how long each takes to be made (the system answers, not the proxy): in `idle`, 0.69 ms at the median, 1.14 ms at the
95th percentile and 1.32 ms at the 99th; in `active`, 0.84 ms, 1.45 ms and **1,005 ms**. That last connection took one second: one connection in 50 took
as long as the first retransmission of a connection request, which is consistent with one packet lost on the way, and says nothing of why.
The estimate of the network's share that the bench writes (`network_estimate_us`, twice the median above) was 1,670 µs against a total of 819 µs measured through the proxy:
it **overestimates**, because making a connection is slower than an exchange of data on the same link, and `added_less_network_p50_us`
(−919 µs) means nothing. It should not be used.

### This machine

The bench used 66 % of a core on average in the measured window (4 % while the connections were held), and 167 MiB of resident memory at most. The free memory never went under 30.9 GiB.

## What the proxy's machine saw

Read on machine B by another agent during the run, every 5 seconds: a peak below is the highest mean over 5 seconds, not an instant.
The times are those of machine A's clock (machine B's is ahead of it, see below). The processor figures are percent of one core: 100 is one
core, and the service was limited to 600.

### The proxy's processor, memory and threads

| Phase | Proxy processor, peak / mean | Threads | Resident memory |
| --- | --- | --- | --- |
| Before the run | | 9 | 16.6 MiB |
| idle, wave 1 join | 273 % / 158 % | 385 | 86 MiB |
| idle, wave 2 join | 289 % / 163 % | 760 | 143 MiB |
| idle, hold (250 players, 30 s) | 4.0 % / 1.6 % | | 147 MiB |
| active, wave 1 join | 270 % / 188 % | 385 | 130 MiB |
| active, wave 2 join | 285 % / 193 % | 760 | 164 MiB |
| active, measure (250 players, 30 s) | 120 % / **119 %** | | |
| Highest of the run | | 760 | 164.7 MiB (the service's cgroup: `memory.peak` 251 MiB) |
| 18:09:00, the 250 of `idle` gone | | | 117 MiB |
| 18:11:00, the 250 of `active` gone | | | 116 MiB |
| 18:12:30 to 18:21:30, no player (109 readings) | | 9 | **135.1 MiB, the same at every reading** (138,296 KB; the cgroup's `memory.current` 136.2 to 136.8 MiB) |

The proxy used up to 2.9 cores of the 4 (8 threads) during a join, and about 1.2 cores for 250 playing players. The threads are three for each player and nine besides.

### The machine

| Phase | Host processor, user / system / softirq, peak | Network received, peak / mean | Network sent, peak / mean | TCP retransmissions |
| --- | --- | --- | --- | --- |
| idle, wave 1 join | 31.8 / 1.0 / 4.4 % | 95.3 / 55.0 Mbit/s | 70.2 / 53.5 Mbit/s | +6,096 |
| idle, wave 2 join | 34.5 / 1.6 / 4.1 % | 96.5 / 55.1 Mbit/s | 74.3 / 54.0 Mbit/s | +5,689 |
| active, wave 1 join | 31.1 / 2.5 / 4.9 % | 95.7 / 64.5 Mbit/s | 86.0 / 61.7 Mbit/s | +4,768 |
| active, wave 2 join | 30.7 / 4.3 / 5.7 % | 94.9 / 65.2 Mbit/s | 92.0 / 62.1 Mbit/s | +5,542 |
| active, measure | user 8.7, system 4.4 (peak 11.2), softirq 4.2 (peak 8.3) % | 30.8 Mbit/s mean | 30.7 Mbit/s mean | +639 |
| idle, hold | | about 0 | about 0 | |

Over the whole run: no stolen time, no listen overflows or drops, no throttling of the service by its cgroup, and no errors, drops or
CRC errors on the interface. The highest temperature was 52.9 °C. The packets received during a join were 95 to 96.5 Mbit/s of 100, about 98 % of what the
link can carry, and about 5,000 TCP segments were sent again in each join. That fits a link that was full, which is what the 11 to 15 s of the joins
suggest; it was **not checked** (a run at 1 Gbit/s would show it), and it is the other agent's reading as much as ours.

### The proxy's own counters

At the end of the run: 740 connections in all (140 before, 600 in this run: 250 and 250 players, and 100 empty ones), at most 250 players at once,
none turned away (`throttled`, `full`, `no_route`, `banned` all 0), no error in the log, and 200 lines "client closed the connection", which are the
empty connections that the bench opens to time the network (four steps of 50, two of them in the ten-bot run before this one). The log shows no slow join.

### Defects the readings show, not explained

- **The page of metrics did not answer in time during the joins.** It missed its 3-second deadline six times, every time during a join, 5 to 8 seconds after the logins were over, while the
  proxy was relaying the worlds. Outside the joins it answered. The cause is not known; it is a defect of the proxy to look into, not something this page explains.
- **The memory is not given back.** The resident memory was 16.6 MiB before the run and 164.7 MiB at the highest, 117 MiB at 18:09:00 and 116 MiB at 18:11:00, once the players of each step had left.
  It then rose to 135 MiB between 18:11:00 and 18:12:30, for a reason not known, and stayed at 135.1 MiB, not moving at all, for the nine minutes until 18:21:30, with no player and nine threads.
  About 118 MiB were held for nothing. At the end the cgroup's `memory.stat` gave 129.3 MiB of anonymous memory, no file memory, 3.9 MiB of slab and 2.0 MiB of sockets, and `memory.peak` was 251.0 MiB.
  That is **not** a slow return of memory: it does not come back. It is compatible with an allocator that keeps its pages after about 760 threads have come and gone (with many threads the allocator
  may spread them over several arenas, and an arena does not always give its free pages back; this was not checked), and it cannot be told from a leak with this run. Telling them apart takes either a run
  with the number of arenas limited (`MALLOC_ARENA_MAX=2`) against one without, or two runs one after the other on the same proxy, where the resting level would not rise again if it is the allocator.
  The baseline had seen memory that was not given back on one machine as well ([BASELINE.md](BASELINE.md)); neither run tells which it is.

## What this run does not say

- **The link was very probably the limit during the joins.** A 100 Mbit/s adapter carried 125 worlds in 11 to 15 seconds, the latency of the players already in went to half a second at the same time,
  and the proxy's machine received at 95 to 96.5 Mbit/s with about 5,000 segments sent again in each join, while its processor was at 2.9 cores of 4 and nothing was throttled. The
  figures of the joins, of the login times of the tail and of the wave effect are those of this link. That the link was the cause, and the only one, is a reading, not a test: a run on a link of 1 Gbit/s would show it.
- **The servers are synthetic.** They send the kinds and sizes of packets of a game server, with made-up chunks that compress like chunks, and do no game work. The players are offline: no encryption and no
  session service. See [BASELINE.md](BASELINE.md) for what that leaves out.
- **One run.** There was no repetition, so there is no spread to give; the wave effect rests on 938 round trips, and the one-second connection on one connection in 100.
- **Only 250 bots, and only two waves.** Nothing is known here of 500 or of more waves, and the run should not be extended to them from these figures.
- **The clocks.** The clock of machine B was ahead of machine A's by 602 ms at 17:30, 641 ms at 18:02, 645 ms at 18:06:31 and 652 ms at 18:12:30 (UTC), a drift of about 1.2 ms a minute: about 646 ms at 18:07:15 and
  649 ms at 18:10:00. The times in this page are those of machine A, and the readings of machine B were given in those times, with that correction. Whoever reads machine B's own log subtracts about 0.65 s.
- **The network between the machines was only read on machine B's side** (the counters of the interface and of TCP): nothing was measured on machine A, and no queue length anywhere. The one-second connection
  fits the retransmissions that machine B counted, and nothing says that it was one of them.
- **The proxy's readings are every 5 seconds**, so a peak of a few seconds is a mean, and a join of 11 to 15 s has two or three of them.

## Against the baseline

[BASELINE.md](BASELINE.md) measured the same bots against a proxy on the machine that held them, over loopback, with 250 bots all at once. What can be set side by side, and what cannot:

| | Baseline, 250 bots, one machine | This run, 250 bots in two waves, two machines |
| --- | --- | --- |
| Load that the bots put on the proxy in the measured window | 8.71 MB/s down | 8.70 MB/s down |
| Round trip through the proxy, p50 and p99 | 131 µs and 239 µs | 819 µs and 2,833 µs |
| Added by the proxy, p50 and p99 | +66 µs and +122 µs | +751 µs and +2,718 µs |
| Time to give the 250 their worlds | 250 logins at once, about 168 a second | 125 worlds in 11 to 15 s, twice |
| Threads of the proxy | 762 (12 + 3 for each player) | 760 (9 + 3 for each player) |
| Processor of the proxy for a playing player | 0.265 % of a core, on the i9 | 0.48 % of a core (119 % for 250 playing players), on the Ryzen 5 3500U |

The first row says that the load was the same, which is what makes the rest comparable at all. The threads agree: three for each player. The second and third rows are **not** a comparison of the proxy: the baseline had no network and an
i9 for the proxy, this run had a network and a laptop processor, and the added time of this run is mostly the network. The fourth is the link. The last row sets two different machines side by side, an i9 and a laptop Ryzen:
it does not say that the proxy costs almost twice as much on the second, and this page does not claim that the two are equivalent either.
