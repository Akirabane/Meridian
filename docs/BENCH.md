**Local measurements, on a shared machine, with no outside comparison. Do not quote
them as the proxy's performance in production.**

# Benchmark

`meridian-bench` measures the proxy on one machine, over 127.0.0.1, against a direct
connection to the same stand-in backend. It is the harness of DESIGN.md 17.2 for the
criteria it covers; this page says how it works, what it does not cover, and what it
measured.

No other proxy was measured, and no figure from elsewhere is used.

## Running it

```
cargo build --release -p meridian-proxy -p meridian-bench
target/release/meridian-bench --players 10,100 --out report.md
```

| Option | Default | Meaning |
| --- | --- | --- |
| `--players LIST` | `10,100` | Player counts, one step each |
| `--seconds N` | `20` | The measured window of each phase |
| `--rate N` | `20` | Measuring packets a second, per player |
| `--size N` | `64` | Bytes of data in each measuring packet |
| `--proxy PATH` | next to `meridian-bench` | The `meridian` program to measure |
| `--out PATH` | none | Also write the report to this file |

The report goes to the standard output (and to `--out`): the command, the date, the
machine, the commit and the build, the load, the method, then one table per step. A
step of 1,000 players opens about 3,000 connections on loopback and runs about 4,000
threads; plan larger steps before running them.

The run exits with 0 when every step ran and the proxy was gone after each, 1
otherwise, 2 on a wrong option.

## What a step does

1. A stand-in backend starts on 127.0.0.1. It logs each player in as a Horizon server
   does, ends the configuration phase, then sends every measuring packet straight
   back. It does nothing else, so the proxy is measured and not a game server.
2. **Direct phase.** N players log in to the backend directly, all at once. When all
   have logged in or failed, the measured window opens for all of them together: each
   player sends a measuring packet, waits for its echo and times it, then waits for
   its next turn (`--rate` a second).
3. A second backend starts, this time asking for the proxy's player information
   forwarding, and the real `meridian` program starts as a child process in front of
   it: offline logins, no player limit, no connection rate limit, compression at 256
   bytes on both sides, as a server asks by default.
4. **Proxied phase.** The same N players, the same packets, the same rate, through the
   proxy.
5. The proxy is stopped and the run checks that its process is gone.

A measuring packet is a play-phase custom payload on the channel `meridian:bench`,
carrying a sequence number and filler up to `--size` bytes. The relay passes it as it
is: the test `the_proxy_relays_measuring_packets_untouched` runs the proxy's relay with
such packets both ways and checks they arrive byte for byte.

## What the report shows

| Row | How it is measured |
| --- | --- |
| Logged in, Logins the backend completed | Players whose login ended in the play phase, as each side counted them |
| Logins a second | Players logged in, divided by the time from the first connection to the last login |
| Login, median and p95 | From the TCP connection to the end of the configuration phase, per player |
| Round trip, median, p95, p99, max | Per measuring packet, from its sending to its echo; nearest-rank percentiles |
| Added | The proxied figure minus the direct one, for the same row |
| Packets and bytes a second | Measuring packets sent by the players over the window, each way |
| CPU | Each process's own processor time over the window, in percent of one core |
| Rest of the machine's CPU | The whole machine minus this program and the proxy |
| Proxy memory | The proxy's resident size before any player, and at the end of the window |
| Failures | Players that could not log in or broke off, with the first reasons; running out of ports or descriptors is counted apart |

## Limits

- **One machine, shared.** The players, the backend and the proxy share the same
  cores, and other programs run too: the report shows how busy the rest of the
  machine was. Loopback hides network latency and losses.
- **Logins come in a burst.** All the players of a step connect at once, so the login
  figures are those of a burst, not of players arriving one by one.
- **The proxy polls its listener every 50 ms.** When no connection is waiting, the
  accept loop of `meridian-proxy` (`serve.rs`) sleeps 50 ms, so a login waits between
  0 and 50 ms before the proxy reads it. Each phase starts after a random pause, so a
  run does not line up with that cycle the same way every time; the login figures
  include this wait, and in a large burst connections are left waiting in the
  listener's queue or tried again by the client's system.
- **CPU on Windows.** Windows charges a process's processor time by clock tick,
  about every 15.6 ms, to whichever thread runs at that moment; the short bursts of
  work of a run fall between ticks and are mostly missed (at 100 players the proxy
  read as 0 %). On Windows the bench reads each process's processor cycles instead
  (`QueryProcessCycleTime`) and turns them into time with the cycles a second of one
  busy core, taken from a busy loop at the start of the run. This is an estimate,
  not an exact processor time. Elsewhere the system's own figure is used. The
  Windows calls are the only unsafe code of the workspace, in
  `crates/meridian-bench/src/cpu.rs`.
- **Not covered yet** (DESIGN.md 17.2): players moving and chatting, switches between
  backends, the login modes other than offline and the lookup cache, failures (dead
  backend, slow client, half-open connections), the ghost's memory, and Linux runs.

## Results

The reference run, as the report gave it (the `--out` path shortened). During it, the
machine's free memory stayed above 28 GiB and at most about 4,800 loopback ports were
in TIME_WAIT, out of 16,384 ephemeral ports.

- Command: `D:\disque\dev\meridian-target-bench\release\meridian-bench.exe --players 10,100,500,1000 --out bench.md`
- Date: 2026-11-02T18:23:20Z
- Machine: Windows 11 (26200), Intel(R) Core(TM) i9-14900KF, 32 logical cores, 63.8 GiB of memory
- Machine load before the run: 341 % of one core in use, out of 3200, and 30.8 GiB of memory available
- Commit: c8c39a0 (release build)
- Load: 10, 100, 500, 1000 players in steps, each sending 20 measuring packets a second of 64 bytes for 20 s once all are logged in

### 10 players

| | Direct | Through the proxy | Added |
| --- | --- | --- | --- |
| Logged in | 10 of 10 | 10 of 10 | |
| Logins the backend completed | 10 | 10 | |
| Logins a second | 4899 | 455 | |
| Login, median | 1.45 ms | 20.93 ms | +19.49 ms |
| Login, p95 | 1.64 ms | 21.70 ms | +20.06 ms |
| Round trip, median | 84.1 µs | 150.6 µs | +66.5 µs |
| Round trip, p95 | 157.6 µs | 240.0 µs | +82.4 µs |
| Round trip, p99 | 211.3 µs | 287.1 µs | +75.8 µs |
| Round trip, max | 306.7 µs | 470.5 µs | +163.8 µs |
| Round trips measured | 4000 | 4000 | |
| Packets a second, each way | 200 | 200 | |
| Bytes a second, each way | 16000 | 16000 | |
| This program's CPU (players and backend) | 2.0 % | 2.1 % | |
| Rest of the machine's CPU | 344.6 % | 449.5 % | |
| Proxy CPU | | 1.4 % (0.138 % a player) | |
| Proxy memory | | 9.8 MiB idle, 10.7 MiB loaded (101 KiB a player) | |

Proxy log lines at warning level or worse: 0. The proxy process was gone after the step.

### 100 players

| | Direct | Through the proxy | Added |
| --- | --- | --- | --- |
| Logged in | 100 of 100 | 100 of 100 | |
| Logins the backend completed | 100 | 100 | |
| Logins a second | 10150 | 1851 | |
| Login, median | 2.97 ms | 46.45 ms | +43.48 ms |
| Login, p95 | 6.74 ms | 49.03 ms | +42.28 ms |
| Round trip, median | 53.6 µs | 128.9 µs | +75.3 µs |
| Round trip, p95 | 199.3 µs | 378.5 µs | +179.2 µs |
| Round trip, p99 | 310.2 µs | 574.9 µs | +264.7 µs |
| Round trip, max | 652.5 µs | 1313.5 µs | +661.0 µs |
| Round trips measured | 40000 | 40000 | |
| Packets a second, each way | 1999 | 1999 | |
| Bytes a second, each way | 159946 | 159945 | |
| This program's CPU (players and backend) | 13.1 % | 14.1 % | |
| Rest of the machine's CPU | 354.9 % | 342.7 % | |
| Proxy CPU | | 12.4 % (0.124 % a player) | |
| Proxy memory | | 9.8 MiB idle, 12.2 MiB loaded (25 KiB a player) | |

Proxy log lines at warning level or worse: 0. The proxy process was gone after the step.

### 500 players

| | Direct | Through the proxy | Added |
| --- | --- | --- | --- |
| Logged in | 500 of 500 | 500 of 500 | |
| Logins the backend completed | 500 | 500 | |
| Logins a second | 11379 | 905 | |
| Login, median | 15.16 ms | 42.75 ms | +27.59 ms |
| Login, p95 | 25.67 ms | 528.41 ms | +502.74 ms |
| Round trip, median | 69.8 µs | 152.0 µs | +82.2 µs |
| Round trip, p95 | 189.4 µs | 392.2 µs | +202.8 µs |
| Round trip, p99 | 554.4 µs | 1080.5 µs | +526.1 µs |
| Round trip, max | 1846.4 µs | 5102.3 µs | +3255.9 µs |
| Round trips measured | 200000 | 200000 | |
| Packets a second, each way | 9977 | 9979 | |
| Bytes a second, each way | 798193 | 798337 | |
| This program's CPU (players and backend) | 77.9 % | 80.0 % | |
| Rest of the machine's CPU | 468.3 % | 405.0 % | |
| Proxy CPU | | 73.5 % (0.147 % a player) | |
| Proxy memory | | 9.8 MiB idle, 27.9 MiB loaded (37 KiB a player) | |

Proxy log lines at warning level or worse: 0. The proxy process was gone after the step.

### 1000 players

| | Direct | Through the proxy | Added |
| --- | --- | --- | --- |
| Logged in | 1000 of 1000 | 1000 of 1000 | |
| Logins the backend completed | 1000 | 1000 | |
| Logins a second | 11078 | 478 | |
| Login, median | 26.93 ms | 593.99 ms | +567.06 ms |
| Login, p95 | 38.69 ms | 1553.12 ms | +1514.43 ms |
| Round trip, median | 66.9 µs | 160.1 µs | +93.2 µs |
| Round trip, p95 | 170.6 µs | 314.5 µs | +143.9 µs |
| Round trip, p99 | 248.3 µs | 668.3 µs | +420.0 µs |
| Round trip, max | 1978.1 µs | 7933.0 µs | +5954.9 µs |
| Round trips measured | 400000 | 400000 | |
| Packets a second, each way | 19884 | 19900 | |
| Bytes a second, each way | 1590737 | 1592038 | |
| This program's CPU (players and backend) | 156.6 % | 177.5 % | |
| Rest of the machine's CPU | 501.2 % | 471.6 % | |
| Proxy CPU | | 158.8 % (0.159 % a player) | |
| Proxy memory | | 9.8 MiB idle, 72.1 MiB loaded (64 KiB a player) | |

Proxy log lines at warning level or worse: 0. The proxy process was gone after the step.

### What this run shows

These are observations of one run on one machine, not targets met or promised.

- Every player of every step logged in and was relayed, up to 1,000; the proxy logged
  no warning and its process was gone after each step.
- The relay added between 66 and 93 µs to the median round trip, and between 76 and
  526 µs to the p99, from 10 to 1,000 players.
- The proxy used about 0.12 to 0.16 % of one core a player at 20 packets a second
  each way, a little more per player at 1,000 than at 100. Its resident memory went
  from 9.8 MiB idle to 72 MiB with 1,000 players.
- A burst of logins is slow through the proxy: at 1,000 players the p95 login took
  1.55 s against 39 ms directly, and 478 logins a second were taken against 11,078.
  The waits of about 0.5 and 1.5 s are those of connections tried again by the
  client's system, which fits the proxy's listener filling its queue while the
  accept loop sleeps its 50 ms (see Limits). This is a fault of the proxy, to be
  fixed apart and measured again with this bench.
