**One valid run of 250 bots, with one real client in the game, on a shared four-core VPS, against synthetic servers on the
same machine. It says what this proxy cost there on 5 November 2026. It is not the proxy's limit, it was not repeated for the
reference configuration, and the bots and the proxy shared the machine.**

# 250 bots and a real client on a proxy on a VPS

After [LOAD-LAN-250.md](LOAD-LAN-250.md), where the proxy was a laptop on a home network and the link of 100 Mbit/s was the limit, this is
the same load on a small server that has a good link: the proxy, the bots, the synthetic servers and a real Horizon server all run on **one VPS**,
and the only thing that comes from outside is one real player's client. The bench is the one of
[LOAD-REMOTE.md](LOAD-REMOTE.md), run on the VPS against `127.0.0.1`. The proxy's own figures were read on the same machine, with the same clock.

What the player felt is not in this page; it is in [REAL-CLIENT-RUNS.md](REAL-CLIENT-RUNS.md).

## The machine and the montage

| | |
| --- | --- |
| The VPS | An OVH VPS in Warsaw, 4 vCores (Intel Core, Haswell generation, no TSX, as the system names it), 7.6 GiB of memory, Ubuntu 26.04, a public link of 1 Gbit/s. Other customers share the host |
| The proxy | `meridian` from commit `e1e2a0e6` of `main`, built by the CI (Linux, release), a systemd service held to **cores 0-1** (`AllowedCPUs`), 3 GB of memory, no connection limit, offline mode, compression threshold 256 on both sides |
| The bots and the synthetic servers | `meridian-bench`, the same archive, started by `systemd-run` on **cores 2-3**, in the loopback: the bots connect to `127.0.0.1:25565`, the synthetic server listens on `127.0.0.1:25701`. No world of the bots crosses the VPS's network card |
| The real server | `horizon-server` of the Horizon Nightly `66f3981`, a service on cores 2-3 with a high processor weight (`CPUWeight=500`), loopback port 25566, offline, the forwarding secret of the proxy |
| The real client | The Horizon client of the same Nightly, on the user's PC, at the VPS's public address; the firewall let in that one address and, at that time, only SSH besides |
| Routing | `default-backend` is the real server; `[[forced-host]]` `127.0.0.1` leads to the synthetic one. The bots write `127.0.0.1` in their handshake, the client types the public address |
| Readings | the bench's own CSV files; every 5 seconds the proxy's cgroup (processor, memory, tasks), the machine (processor, steal, pressure, load, network interface, sockets), the proxy's metrics page and, for the real server, its status |

There was no sidecar and no panel. The bots go through the loopback, so they use no port of the firewall: it let in SSH and, for the client, the proxy's port from the user's address only.

The bench ran as in [LOAD-REMOTE.md](LOAD-REMOTE.md):

```
meridian-bench load --proxy 127.0.0.1:25565 --backend-bind 127.0.0.1 --backend-ports 25701-25710 \
  --remote-config <the proxy's configuration> --players 250 --waves 5 --wave-gap-seconds 12 --direct-waves 5 \
  --seconds 30 --direct-seconds 20 --max-total-seconds 1500 --max-lost-percent 100 --min-free-gib 2 \
  --out-dir <folder> --label "<name of the run>"
```

The bots are those of [BASELINE.md](BASELINE.md): a world of 169 chunk columns and 30 entities for each (about 2.7 MB before compression), 20 small
packets and 2 chunk packets a second, 20 movement packets and 2 measuring packets a second up. The run has two steps, `idle` (30 s of held
connections) and `active` (a direct phase to the synthetic server, 20 s measured, then the phase through the proxy, 30 s measured), and in each
the 250 bots arrive in **5 waves of 50**, 12 seconds after the last bot of the wave before is in.

### The runs

| Run | What it was | Used here |
| --- | --- | --- |
| A | 2 waves of 125, 30 s apart; the first Linux run of the bench. Its direct phase brought 250 bots at once and lost 52 of them, which the old `failed` column hid among the phase through the proxy's zeros; the VPS agent stopped before the next run | Not used. It led to the change of the bench that gives the direct phase its own columns and waves (`fix/bench-direct-phase`). Its proxy figures are in the table of the waves of 125 below |
| C | 2 waves of 125, with the real client; the bench's own guard stopped it after 45 s ("a reading that should take a second took 6.0 s: the machine or this program is starved") | Only for what it shows of a wave of 125 |
| D | 5 waves of 50, the real client connected in the first phases and **gone at 21:05:53 UTC, 13 s into the phase through the proxy** (the real server logged its disconnection; the files do not say why), so the measured window of the phase through the proxy had **no client** | As a second run of the bots' side, with that reserve |
| E | 5 waves of 50, the real client connected **without a break from before the first bot to after the last** | **The reference run** |

The reference run E began at 21:13:26 UTC and ended at 21:18:29 UTC (five minutes, three seconds).

## What the bots saw (run E)

### The phases

All times UTC, one clock for the bench and the proxy.

| Step | Phase | Start | End | Length |
| --- | --- | --- | --- | --- |
| idle | join, waves 1 to 5 | 21:13:27.0 | 21:14:30.7 | 3.0 to 3.2 s each, 12 s apart |
| idle | hold | 21:14:33.7 | 21:15:03.7 | 30 s |
| active, direct | join, waves 1 to 5 | 21:15:07.8 | 21:16:16.3 | 3.2, 3.6, 3.9, 4.4, 5.3 s |
| active, direct | measure | 21:16:21.3 | 21:16:41.4 | 20 s |
| active, through the proxy | join, waves 1 to 5 | 21:16:41.4 | 21:17:51.1 | 3.2, 3.8, 4.2, 4.7, 5.6 s |
| active, through the proxy | measure | 21:17:56.1 | 21:18:26.2 | 30 s |

A join is the time from the first bot of a wave starting to the last one having its world; it grows with the wave as the players already in
receive their stream.

### Bots in and bots lost

All 250 bots logged in and got their world in each step (50 of 50 in each wave), none failed, none was cut off, in the direct phase as in the phase
through the proxy. The 1,000 keep-alives of `idle` were all answered. The guards of the bench did not trip (`added_valid` is true).

### Login times

The time to log in is from the bot connecting to the end of the configuration phase; it **does not include the world** (the joins above do).

| Step, wave | p50 | p95 | p99 |
| --- | --- | --- | --- |
| idle, wave 1 | 56.3 ms | 299.9 ms | 311.3 ms |
| idle, wave 2 | 53.0 ms | 116.9 ms | 147.6 ms |
| idle, wave 3 | 48.6 ms | 97.3 ms | 121.5 ms |
| idle, wave 4 | 37.8 ms | 183.4 ms | 201.0 ms |
| idle, wave 5 | 54.9 ms | 118.9 ms | 130.2 ms |
| active through the proxy, wave 1 | 59.1 ms | 132.6 ms | 155.6 ms |
| active through the proxy, wave 2 | 66.9 ms | 111.9 ms | 129.1 ms |
| active through the proxy, wave 3 | 57.8 ms | 107.6 ms | 140.8 ms |
| active through the proxy, wave 4 | 57.8 ms | 125.7 ms | 155.2 ms |
| active through the proxy, wave 5 | 49.8 ms | 238.5 ms | 491.8 ms |
| all 250 of `idle` | 53.0 ms | 130.2 ms | 299.9 ms |
| all 250 of `active`, through the proxy | 55.6 ms | | 238.5 ms |
| all 250 of `active`, direct | | | 588.2 ms |

### The bots already in, while a wave arrives

The `active` bots of the earlier waves sent two measuring packets a second all through. Their round trips through the proxy, at the 99th percentile,
in the ten seconds before a wave began, while it joined, and in the ten seconds after (microseconds), with the worst one while it joined and the
number of round trips behind the figure:

| Wave arriving | Before | While it joins | After | Worst while it joins | Round trips |
| --- | --- | --- | --- | --- | --- |
| 2 (50 bots in) | 1,780 | 24,310 | 2,098 | 32,253 | 370 |
| 3 (100 bots in) | 2,640 | 17,062 | 3,962 | 35,471 | 848 |
| 4 (150 bots in) | 3,706 | 17,915 | 3,769 | 84,918 | 1,429 |
| 5 (200 bots in) | 3,621 | 18,455 | 4,568 | 129,383 | 2,232 |

For the 3 to 5 seconds in which fifty worlds cross, the players already in have a latency of about 17 to 24 milliseconds at the 99th percentile instead of
2 to 4, with a worst round trip of 32 to 129 ms, and it is back as soon as the worlds are in. There are 370 round trips behind the first row and 2,232
behind the last: the first rows are rougher.

### Steady state

With the 250 bots in, over the 30 s window (15,014 round trips):

| | Direct (same machine) | Through the proxy | Added |
| --- | --- | --- | --- |
| p50 | 60 µs | 245 µs | +185 µs |
| p95 | 783 µs | 2,437 µs | +1,654 µs |
| p99 | 2,221 µs | 4,839 µs | +2,618 µs |
| max | | 11,698 µs | |

Everything is in the loopback: **there is no network in these figures**, nor any in what the proxy "adds". They are the cost of one more process in
the path, **on a machine whose cores the bench also uses**. The direct phase's own 99th percentile (2.2 ms, against 115 µs in the LAN run) suggests how
busy the cores of the bench were. `network_estimate_us` (218 µs, twice the median of the time to connect) is larger than the median added time,
and `added_less_network_p50_us` is −33 µs: the estimate is meaningless here, as it was in the LAN run.

### Throughput

The bots count 8.68 MB/s received in the window (6,017 packets a second down, 5,500 up), as in the LAN run (8.70), so the load put on the proxy was the same. The
network card of the VPS carried **0.1 Mbit/s** on average during that window and **at most 1.4 Mbit/s** while the first bots joined: that is the real client and the SSH sessions of the people operating the machine, nothing of the bots.

### This program

The bench used 92 % of one core on average in the measured window (4 % while the connections were held), and 346 MiB of resident memory at most, on two cores
that the real server also used (0.11 core).

## The second run, without the client in the measured phase (run D)

The client left 13 s into the phase through the proxy, after its first wave and before its second. The bots' side, side by side with run E:

| | Run E (client throughout) | Run D (client gone) |
| --- | --- | --- |
| Bots in, lost, cut off | 250, 0, 0 | 250, 0, 0 |
| Round trip through the proxy p50 / p95 / p99 / max | 245 / 2,437 / 4,839 / 11,698 µs | 242 / 2,062 / 3,846 / 17,805 µs |
| Direct p50 / p99 | 60 / 2,221 µs | 64 / 2,767 µs |
| Added p50 / p95 / p99 | +185 / +1,654 / +2,618 µs | +178 / +971 / +1,079 µs |
| Login p99 of all 250 of `idle` | 299.9 ms | 804.6 ms |
| Login p99 of all 250 of `active` through the proxy | 238.5 ms | 186.4 ms |
| Bots already in, 99th percentile while a wave joins (waves 2 to 5) | 24,310, 17,062, 17,915, 18,455 µs | 28,580, 13,060, 17,663, 27,179 µs |
| Proxy processor, whole window of the measure | 0.78 core mean, 0.95 peak | 0.74 mean, 0.86 peak |
| Proxy `memory.peak` | 226.6 MiB | 204.0 MiB |

The two runs agree on the median and on the order of the rest, and differ by a factor of two to three on a tail (the login p99 of `idle`, the added 99th percentile):
that is the spread to expect between two runs of this kind here. The client's presence is not what separates them, but nothing here can show it either way.

## What the proxy's machine read (run E)

Every 5 seconds, so a wave of 3 to 5 seconds has one or two readings and a peak is a mean over 5 seconds. The processor figures are cores (1.0 is one core; the service had two).

### Processor, memory and tasks of the proxy

| Phase | Proxy processor, mean | Memory of the cgroup, highest | Tasks (threads), highest |
| --- | --- | --- | --- |
| idle, join, waves 1 to 5 | 0.83, 0.92, 0.86, 1.09, 1.10 | 163, 170, 179, 193, 212 MiB | 162, 312, 462, 612, 762 |
| idle, hold | 0.01 | 212 MiB | 762 |
| active, direct (the proxy does nothing) | 0.01 | 164 to 175 MiB | 12 to 162 |
| active, through the proxy, join, waves 1 to 5 | 0.74, 0.87, 0.83, 1.09 (peak 1.49), **1.61** | 175, 184, 194, 208, 224 MiB | 162, 312, 462, 612, 762 |
| active, through the proxy, measure (250 playing) | **0.78** (peak 0.95) | 224 MiB | 762 |
| Whole run (five minutes) | 0.37 (115.1 s of processor in 309.4 s) | `memory.peak` **226.6 MiB** | 762 |

- **The cost of a playing player** is 0.78 core for 250, **0.31 % of a core each**; at the joins, a wave of 50 takes 0.74 to 1.61 cores for 3 to 6 seconds, and the cost
  of a join grows with the players already in. The service never reached its limit of two cores (peak 1.61), was never throttled (`nr_throttled` 0).
- **The threads** are three for each player and twelve besides: 762 for 250, as in the baseline and in the LAN run.
- **The memory does not come back.** The cgroup held 150 MiB when run E began (the service had not been restarted since the runs before) and 181 MiB when the 250 bots had
  left, and 124 MiB at the start of run D, 150.5 MiB at its end. The cgroup's memory counts kernel memory too, so it is not the process's resident memory of the LAN page, and
  the number of runs on this service is too small to tell a leak from an allocator that keeps its pages ([LOAD-LAN-250.md](LOAD-LAN-250.md) saw the same).
- **Sockets.** At most 1,019 sockets in the established state at the same time on the machine, which counts both ends of each loopback connection (the bots, the proxy and the
  synthetic servers), plus the real client's: it is not a count of players.

### The machine

- The processor of the host was never short in the mean, but its pressure was: **at least one task was waiting for a core up to 57.6 % of the time** (`some`, a 10-second average) in the worst reading,
  and the load average went up to 21 on four cores: the bench runs several threads for each bot, and they share two cores.
- **Stolen time** was between 0.00 and 0.10 % in every phase (0.055 % over the run): the host's other customers did not take the VPS's cores in these minutes.
- The proxy's page counted no connection turned away (`throttled`, `full`, `no_route`, `banned` all 0), no fallback, and there is no line at error level in the proxy's log. 100 lines at warning level, all of one
  kind and 20 ms apart, are the 2 × 50 empty connections that `raw_connect` opens to time the connection (the counters are cumulative: the service was not restarted between runs).
- Median time to make a TCP connection to the proxy, in the loopback: 108 µs in `idle`, 109 µs in `active`, 99th percentile 173 and 219 µs.

### The real server

During the 31 readings of its status in run E, `horizon-server` showed 20.0 ticks a second throughout, 4.6 to 6.1 ms of tick time on average (95th percentile at most 8.6 ms), one player,
719 chunks loaded, 0.45 to 0.46 GB of memory and 9 to 12 % of a core (0.11 core over the run). That is the real server with one player on cores that also ran 250 bots.

## A wave of 125 on two cores (runs A and C)

The same machine, the proxy held to two cores, and the bots arriving in two waves of 125:

| | Run A | Run C |
| --- | --- | --- |
| Proxy processor at its peak | 2.16 cores (wave 1) | 1.80 cores (wave 2) |
| `idle`, login of the first / second wave | p50 786 / 1,343 ms, p99 3,456 / 4,219 ms | p50 107.5 / 229.5 ms, p99 4,407.4 / 1,278.8 ms |
| `active`, login of the first / second wave | p50 144.8 / 212.3 ms, p99 404.9 / 921.2 ms | no `active` step |
| Bots already in, 99th percentile before / while / after the second wave joins | 1,970 / 52,167 / 2,690 µs (worst 311 ms) | no `active` step |
| Join of a wave | 7.7 s and 10.8 s (`active`) | 7.6 s and 5.7 s (`idle`) |
| The end | all steps ended | the bench stopped at 45 s: one of its own readings took 6.0 s |

A wave of 125 at once makes the proxy use almost all of its two cores for six to ten seconds, and the logins of the tail of the wave wait one to four seconds. Run C stopped because the bench's own
1-second reading took 6 seconds, which is the bench or the machine starved: it cannot be said from these files which of the two cores' users it was. Waves of 50 (above) did not do it.

## What this run does not say

- **The machine is shared in two ways.** The VPS's host has other customers (stolen time was low here, which is no promise), and the proxy, the bench and the real server share four cores: the proxy has two
  of them to itself, the bots and the synthetic servers share the other two with the real server. The bench was at 92 to 94 % of one core on average and the system pressure reached 57 %: **the bots' own timing may
  be late, and the round trips include waits for a core on the bots' side.** How much is not known.
- **There is no network between the bots and the proxy.** The loopback hides everything a real network does (latency, loss, a link that fills, as in the LAN run). The figures of the joins and of the added time here are those of a path without a network.
  The only real network is the one client's, and it is not measured in this page.
- **Synthetic servers.** Their worlds are made up (169 columns of about 2.7 MB with data that compresses like chunks), they do no game work, and the players are offline: no encryption, no session service. See [BASELINE.md](BASELINE.md).
- **One reference run.** Run E is one run; run D is a second, without the client in the measured phase, and they differ by a factor of two or three on tails. There is no run for the configuration of the proxy on three cores, and no
  repetition of E. The wave effect rests on 370 to 2,232 round trips for each wave.
- **One real client**, in the game at one place, and not moving much (see the other page). Many real players are not 250 bots.
- **Never 500 or 1,000** here; nothing in this page says what the proxy does past 250 players, on two cores or on more.
- **The cores of the proxy were limited to two.** A single join of 125 players nearly used them up (runs A and C); 250 in waves of 50 did not (1.61 at the most).
- **The processor figures are every 5 seconds** and the cgroup's memory counts more than the process's resident memory.
- **The service had run before** (the cgroup held 124 to 150 MiB when each run began), so the start was not an empty proxy.

## Against the LAN run

[LOAD-LAN-250.md](LOAD-LAN-250.md) ran the same 250 bots against a proxy on a laptop. **The two are not a ranking**: the hardware, the network and the arrival differ.

| | LAN run (laptop, 100 Mbit/s link, bots on another machine) | This run (VPS, loopback, everything on one machine) |
| --- | --- | --- |
| Proxy's machine | Ryzen 5 3500U, 4 cores / 8 threads, 5.9 GB | 4 vCores (Haswell generation), 7.6 GiB, shared host |
| Arrival | 2 waves of 125 | 5 waves of 50 (and 2 × 125 in runs A and C) |
| Load put on the proxy | 8.70 MB/s counted | 8.68 MB/s counted |
| Threads of the proxy | 760 | 762 |
| Processor of the proxy, 250 playing | 1.19 cores (0.48 % of a core each) | 0.78 core (0.31 % of a core each) |
| Processor at the joins | up to 2.9 cores for 125 | up to 1.61 cores for 50; up to 2.16 and 1.80 for 125 |
| Memory of the proxy, highest | 164.7 MiB resident | 226.6 MiB, the cgroup's (not the same measure) |
| Added by the proxy, p50 / p99 | +751 µs / +2,718 µs, **network included** | +185 µs / +2,618 µs, **no network** |
| Bots lost | 0 | 0 |
| Join of a wave | 11 to 15 s for 125, the link full | 3 to 5.6 s for 50; 6 to 11 s for 125 |
| Round trips of the bots in, while a wave joins | 481 ms (125 on a full link) | 17 to 24 ms (50 in the loopback) |

What can be said: the load was the same; the thread count is the same formula; the cost of a playing player was lower on the VPS's cores than on the laptop's (0.31 against 0.48 % of a core), a difference of
hardware that this page cannot cut into the processor's share and the other loads' share; and the half-second of latency of the LAN run during a join is not seen here, where the joins are smaller, there is no 100 Mbit/s link, and the
latency is what the bench's two cores allow. What cannot be said: that the proxy is "faster" on a VPS, or that waves of 50 are better than waves of 125 for the proxy (the arrival differs, and so does the path).

## Not measured

- The proxy on three or four cores, and the same run repeated: to tell the spread of a run from the effect of a core.
- A bench on another machine, or on cores of its own, against the same proxy: the way to take the bench's cores out of the proxy's round trips.
- A real network between the bots and the proxy at this size (the LAN run had one, at 100 Mbit/s).
- 500 or 1,000 players; many real clients; online mode (the AES cost of [BASELINE.md](BASELINE.md) is not in these figures).
- The cause of the proxy's memory not coming back, and a long run.
