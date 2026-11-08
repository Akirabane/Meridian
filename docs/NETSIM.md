# NETSIM: a relay that makes a link worse

`meridian-bench netsim` is a TCP relay for trying the proxy on a bad network. It listens on one
address, connects each client to another, and degrades what passes: a delay, a jitter, a rate limit,
stalls, and connections closed or reset at random. It writes a log of what it did.

```text
meridian-bench netsim --listen 127.0.0.1:25566 --to 127.0.0.1:25565 --delay 20 --log netsim.jsonl
```

## What it does not do

**It does not lose packets.** TCP does not, and on 127.0.0.1 nothing is lost anyway. A *stall* stops
the relay from reading and from delivering for a while, so the sender's buffers fill and its writes
block; then the data comes in a burst. That is what a loss looks like to a TCP stream from above. It is
not the retransmission, the cut of the congestion window or the reordering that cause it.

It does not simulate MTU and fragmentation, the buffers of a router, the congestion control of a long
path (the senders' windows are those of this machine), NAT, other flows, or an asymmetric route. It adds
a delay to a loopback path; it is not a lab network.

## Options

| Option | What it does |
| --- | --- |
| `--listen ADDR`, `--to ADDR` | where it listens, and where each client is connected to |
| `--delay MS`, `--delay-up MS`, `--delay-down MS` | a fixed delay, each way or one way (up: from the client to the target) |
| `--jitter MS` (and `-up`, `-down`) | each chunk waits `delay` plus or minus a draw up to `jitter`; the order is kept, as TCP keeps it |
| `--rate KBIT` (and `-up`, `-down`) | the most a connection carries each way, in kilobits a second |
| `--stall-every S`, `--stall-for MS` | on average every S seconds a connection's way stalls for MS (default 200 ms); the two ways are drawn apart |
| `--reset-per-min N` | on average N times a minute, a random live connection is closed (a FIN) |
| `--rst` | the closed connection is reset with an RST instead (Windows only in this version) |
| `--seed N` | every random draw comes from it: the same seed, the same degradation |
| `--log FILE` | one JSON line for each event, and a summary line at the end |
| `--seconds N`, `--stop-file FILE` | stop after N seconds, or as soon as the file exists (Ctrl+C also works from a console) |

The options have bounds, and a value outside them is refused when the command line is read, with the
name of the option: a delay or jitter from 0 to 60,000 ms, a rate from 1 to 100,000,000 kbit a second, a
stall of up to 600,000 ms every up to 86,400 s, up to 100,000 resets a minute, `--seconds` from 1 to
604,800, `--seed` a whole number. (`--rate 0` is not "no limit": leave the option out.)

## How

For each connection there is a thread that serves it and, for each direction, a reader and a writer: 5
threads, and 2 descriptors (the two sockets, shared by the threads: nothing is cloned). The reader stamps
each chunk it reads with its arrival time and hands it to the writer, which waits until
`arrival + delay + jitter` (never before the chunk before it) and writes it. A rate limit lengthens
that wait by the time the chunk takes on the pipe; a stall makes the reader stop reading. On Windows the
relay asks the system for a 1 ms timer (the default is 15.6 ms), and the log records by how much each
write missed its time.

**Backpressure.** The reader and the writer share a queue of 64 chunks (16 KiB at most each: about 1 MiB).
When it is full the reader stops reading, as a sender stops on a full TCP window: memory stays bounded
whatever `--rate` is, and a sender that is faster than the rate is slowed down, not buffered. The same
queue means a direction carries at most about 1 MiB per delay (50 MB/s at 20 ms, 10 MB/s at 100 ms): a limit
of the relay, not of the link it stands for.

**Stop.** A stop (Ctrl+C, `--seconds`, `--stop-file`) ends a stall at once, shuts every live connection and
waits up to 5 seconds for their threads to finish; each writes its `close` event, and only then is the
summary counted. The readers look at the stop flag every 200 ms (a read on a socket has that timeout),
because a shutdown does not wake a blocked read on every system.

The relay is meant for **up to 600 connections**: 1,200 descriptors and 3,000 threads. On Linux raise
`ulimit -n` to at least 1,300 first. It says so at the start, and once more on the screen and in the log
if it goes over, instead of slowing down without a word. A connection whose threads the system will not
start is cut and logged (`refused`), without a panic.

## The log

One JSON line for each event, with the time since the relay started (`t_ms`): `start` (the options),
`open` and `close` of a connection, `stall` (the way and its length), `reset` (`fin` or `rst`),
`target-refused`, `refused`, `over-soft-limit`, `stop-timeout`, and at the end `summary`: connections, peak,
bytes each way, stalls, resets, and for each way how long a chunk was held (mean, p99, microseconds) and by
how much its write missed the plan. Each line is written to the file at once, but **a relay that is killed
writes no summary** (and no `close` for the connections it had): only a stop does. Nothing of what the
connections carry is written.

## Use

- Between the bots and the proxy: the bots' `--server` names the relay's address; the proxy sees the same
  players, from 127.0.0.1.
- Between the proxy and a server: the proxy's `address` for that backend is the relay's, and the relay's
  `--to` is the server's. Several relays can run at once, each with its own degradation.

## Measured (8 November 2026)

20 bots, `meridian-bench load --scenarios active` through the proxy (the bench's "through the proxy"
round trip, in microseconds), the relay between the bots and the proxy, on a PC in use (a short step: the
figures show that the relay does what it says, they are not a result about the proxy):

| Relay | Through the proxy, p50 | p99 | Bots in / failed / dropped | What the relay's log says |
| --- | --- | --- | --- | --- |
| none | 139 | 384 | 20 / 0 / 0 | |
| `--delay 20` | 40,209 | 40,434 | 20 / 0 / 0 | |
| `--delay 10 --jitter 5 --stall-every 8 --stall-for 400 --reset-per-min 6` | 20,871 | 362,558 (max 406,445) | 20 / 0 / 5 | 232 stalls, 5 resets; a chunk held 10.1 ms up and 10.5 ms down on average (p99 14.9 and 16.1); writes missed their time by 0.01 and 0.15 ms on average (p99 0.01 and 3.14) |

The delay of 20 ms each way adds 40.07 ms to the round trip (40,209 against 139), and the direct round
trip of the same bots (87 against 86 microseconds) does not move. With stalls of 400 ms the worst round
trip is 406 ms: a stall is felt as a freeze of that length. The 5 resets closed 5 connections, and the
bench counted 5 dropped bots.
