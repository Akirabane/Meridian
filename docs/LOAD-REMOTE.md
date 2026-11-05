# Load against a proxy on another machine

`meridian-bench load` normally starts the proxy itself, on the machine that holds the bots, and reads
its memory, threads and processor time ([BASELINE.md](BASELINE.md)). This page is the other way
round: the proxy runs somewhere else (a service on a Linux machine of the network, say), and the bots
and the synthetic servers stay where the bench runs.

```
bots ──────► proxy (machine B, 192.168.1.25:25565) ──────► synthetic servers (machine A, 192.168.1.18:25701)
  \_____________ machine A ______________/                    \______________ machine A ______________/
```

The bots can arrive all at once or in **waves** (`--waves N --wave-gap-seconds S`): N groups of equal
size, all the bots of a group at once, the next group S seconds after the last of the one before is in.
Only the scenarios `idle` and `active` run this way: a switch (`moves`) needs the proxy's sidecar and
the connection limits are the proxy's own business (`churn`).

## What is measured, and what is not

From machine A, the bots' side:

- how many bots logged in, failed, or were cut; the time a login took; the keep-alives answered;
- the round trip of a measuring packet, through the proxy, and **directly** to the same synthetic
  server on machine A (the direct phase of `active`); their difference is what the proxy "adds";
- `raw_connect_*`: the time to make a TCP connection to the proxy, which is the network between the
  two machines and nothing of the proxy's work, since the system answers it;
- the throughput the bots saw, and this program's own processor and memory.

**What the proxy "adds" now includes the network.** A measuring packet goes from the bot to the proxy,
from the proxy to the synthetic server, and back the same way: it crosses the link between the machines
twice, and the proxy's link to the servers twice. `network_estimate_us` is twice the median of
`raw_connect_*` and `added_less_network_p50_us` is the median added time minus it. It is an estimate
(one connection is a little more than half a round trip of data); read both beside the figures of
[BASELINE.md](BASELINE.md), where the added time had no network in it.

Not from here: the proxy's memory, threads and processor. Whoever runs the proxy reads them on its
machine (`ps`, the cgroup of the service) **during the same windows**. The run writes them to
`windows.csv`:

| column | |
| --- | --- |
| `step` | `idle`, `R2 direct`, `R2 proxy` |
| `players` | bots of the step |
| `phase` | `join` (the bots log in and take the world; with waves, `join wave 1`, `join wave 2`...), `hold` (idle: the connections held), `measure` (active: the measured window, once every wave is in) |
| `start_utc`, `end_utc` | UTC, to the second |
| `start_epoch_ms`, `end_epoch_ms` | milliseconds since the epoch, UTC |
| `seconds` | the length |

The windows to read on the proxy's machine are `idle` / `join` (or each `join wave k`), `idle` / `hold`,
`R2 proxy` / `join` (or each `join wave k`) and `R2 proxy` / `measure`. (`R2 direct` does not touch the proxy.) The two machines' clocks must agree to
about a second: check them (`w32tm /stripchart /computer:<the other machine>` on Windows, `chronyc tracking` or
`timedatectl` on Linux) before reading the windows side by side. (`w32tm /stripchart` only works if the other
machine answers NTP; if it does not, compare `w32tm /query /status` here with `chronyc tracking` there: both should be
synchronised to a time server, with an offset of milliseconds.) The result rows carry the same times
(`window_start_utc`, `window_end_utc`, `window_start_ms`, `window_end_ms`).

## Waves

With `--waves 2 --wave-gap-seconds 30` and 250 bots, 125 bots log in at once, hold for 30 seconds after the
last of them is in, then 125 more arrive while the first 125 play. The rows have, for each wave `k`:
`wk_bots`, `wk_logged_in`, `wk_failed`, `wk_dropped` (cut off), `wk_login_p50/p95/p99/max_ms`, and when it arrived
(`wk_join_start/end_utc` and `_ms`). For the waves after the first, `active` rows also have the figure that
matters: the round trips of the bots **already in** (the earlier waves), at the 99th percentile,
`wk_rest_rtt_p99_before_us` (in the ten seconds before the wave began), `wk_rest_rtt_p99_during_us` (while it was
joining), `wk_rest_rtt_p99_after_us` (the ten seconds after it was in), with the worst one and the number of
round trips behind the figure (`..._max_during_us`, `..._samples_during`). Few samples make a rough figure: a wave that
joins in a second has a few dozen round trips in its window. The round trips are kept with their own times for this; `idle`
bots send no measuring packets, so their rows have the wave columns without the round trips.

A run stops by itself when more than a tenth of the bots (and at least ten) failed to log in or were cut. To
go on and write the losses instead (a test of a limit), give `--max-lost-percent 100`: the failures are then
counted for each wave, and the first reasons are in `first_failure`.

## The direct phase, and what it does to the figures

`active` runs the bots twice: first straight to the synthetic server on the same machine (the direct phase, the reference), then
through the proxy. The direct phase arrives in the same way as the other, in `--direct-waves N` waves (2 by default, with the same
`--wave-gap-seconds`), so that the two are compared on the same arrival. The columns:

- `failed` and `dropped` hold **both phases added together**. The columns of each wave (`wk_*`) are those of the phase through the proxy only.
- `direct_logged`, `direct_failed`, `direct_dropped`, `direct_login_p99_ms`: the direct phase alone. `direct_first_failure` says why its first bots
  failed, and `first_failure` writes the reasons of both phases (the one through the proxy first, then `direct: ...`).
- `added_valid` says whether the direct phase kept every bot (no failure, no cut-off, no guard tripped). When it did not, the
  difference with the phase through the proxy means nothing, so `added_p50_us`, `added_p95_us`, `added_p99_us` and `added_less_network_p50_us` are
  **left empty**, and the screen says so with the first reason. The round trips of each phase on its own are still written.

## Steps

1. **Write the proxy's configuration**, on machine A. It holds a secret of its own, new each time
   (32 hexadecimal characters from the system's random bytes), written in that file and shown nowhere:

   ```
   meridian-bench load --proxy 192.168.1.25:25565 --backend-bind 192.168.1.18 --backend-ports 25701-25710 ^
     --write-remote-config D:\disque\dev\meridian-bench-out\banc-lan\debian-load --meridian <a meridian program>
   ```

   It writes `meridian.toml` (listening on the proxy's address, one backend `lobby` at the first port of the
   range on machine A, offline mode, no connection limits, compression threshold 256 unless `--compression both-off`),
   then runs `meridian --check` on it with the program given to `--meridian` and prints what the
   check said. Use a `meridian` built from the same commit as the proxy that will read the file, or a later one;
   the program's version is printed.
2. **Let the proxy's machine reach machine A.** The system firewall of machine A must let the proxy in on the
   ports of the range, for the program that listens (the bench itself, in this mode; it starts no other
   program). In an administrator's PowerShell:

   ```
   New-NetFirewallRule -DisplayName "meridian-bench LAN load" -Direction Inbound -Action Allow -Protocol TCP `
     -LocalPort 25701-25710 -RemoteAddress 192.168.1.25 `
     -Program "D:\disque\dev\meridian-bench-out\bin-lan\meridian-bench.exe" -Profile Any
   ```

   Remove it when done: `Remove-NetFirewallRule -DisplayName "meridian-bench LAN load"`. The rule is bound to that program path:
   a copy of the bench at another path is not let through, and a rebuilt one must be copied over that file.
3. **Install `meridian.toml` on the proxy's machine** and restart the proxy there (`systemctl restart ...`). Do not
   reuse the file for another run: it holds a secret that the bench read, and a new run makes a new file.
4. **Run**, on machine A, from the copy of the bench the rule names:

   ```
   D:\disque\dev\meridian-bench-out\bin-lan\meridian-bench.exe load --proxy 192.168.1.25:25565 ^
     --backend-bind 192.168.1.18 --backend-ports 25701-25710 ^
     --remote-config D:\disque\dev\meridian-bench-out\banc-lan\debian-load\meridian.toml ^
     --players 100 --seconds 30 --direct-seconds 20 --max-total-seconds 600 ^
     --out-dir D:\disque\dev\meridian-bench-out\banc-lan\run-100 --label "lan 100"
   ```

   The bench reads the secret and the compression from the file, refuses it if it sends `lobby` anywhere but where
   the servers will listen, and writes `idle.csv`, `active.csv` and `windows.csv` in `--out-dir`.
   The usual guards apply (the free memory of machine A, bots lost, a run that lasts too long); the proxy cannot be watched from here,
   so a proxy that went away shows as bots that failed or were cut.

## Before the real run

A run of ten bots, one wave, through the proxy checks the whole path (the firewall, the file, the secret, the clocks) before a large one:

```
D:\disque\dev\meridian-bench-outin-lan\meridian-bench.exe load --proxy 192.168.1.25:25565 ^
  --backend-bind 192.168.1.18 --backend-ports 25701-25710 ^
  --remote-config D:\disque\dev\meridian-bench-outanc-lan\debian-load\meridian.toml ^
  --players 10 --seconds 30 --direct-seconds 10 --max-total-seconds 300 ^
  --out-dir D:\disque\dev\meridian-bench-outanc-lan
un-smoke --label "lan smoke"
```

## Limits

- The network is part of every figure: its latency, and its throughput. A world sent at a join is about 2.7 MB per player,
  about 0.8 MB once compressed, and it crosses the link from machine A to the proxy and back: 100 players joining at once
  move about 160 MB over machine A's network card in a few seconds. A gigabit link carries that; a slow one stretches the
  login times, which are then the network's.
- `raw_connect_*` opens and closes 50 connections to the proxy without a word, one every 20 ms, per step; the proxy may log each as a handshake that ended early.
- The bots and the servers share machine A's processor; this program's own use is in the rows (`bench_cpu_mean_pct`).
- The proxy's machine's own load, other services on it, and its clock, are outside this page.
