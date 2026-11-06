**A leak found by a load test on a rented four-core VPS on 5 and 6 November 2026, its cause, its fix (#119) and the check that the fix holds. The figures are those of that machine and those runs; they are not a limit of the proxy.**

# The memory the proxy kept after its players had left

A proxy that runs for weeks must give back what a player took when the player leaves. This page tells how a
test showed that Meridian did not, how the cause was found, and what was measured before and after the fix. The
short guide for an operator, with the commands, is in [OPERATING.md](OPERATING.md), section 8.2.

## The test

The load tool of [BENCH.md](BENCH.md) ran on the VPS beside the proxy, against synthetic servers in the loopback:

- a **cycle** is 150 simulated players that join in 3 waves of 50, stay 10 seconds, and all leave;
- cycles follow each other 30 seconds apart, with nobody connected between them;
- after each cycle the proxy was read **at rest**: no player, 9 threads (the proxy's own), and the resident
  anonymous memory of the process (`RssAnon` in `/proc/<pid>/status`);
- the VPS had 4 vCores and 7.6 GiB of memory (Ubuntu), and the proxy was a release build.

A proxy that gives back what its players took would show the same figure at rest after every cycle, after a first
rise to its working size.

## What was seen

| Run | Setting | At rest after cycle 1 | At rest after cycle 30 |
| --- | --- | --- | --- |
| A | the default allocator | 58 MiB | 235 MiB |
| B | `MALLOC_ARENA_MAX=2` | 60 MiB | 249 MiB |

Run A rose by about **6 MiB for each cycle** (235 - 58 over 29 cycles), with no plateau: the figure at rest was
higher after each cycle than after the one before, with nobody connected.

## It was not the allocator

The first suspect of a process that does not give memory back is the allocator, which can keep freed memory in its
arenas. Two readings ruled it out:

- Run B limited glibc to 2 arenas (`MALLOC_ARENA_MAX=2`). If arenas had held the memory, it would have stopped
  growing. It grew as in run A (249 MiB after 30 cycles).
- At the end of each run, `malloc_trim` was called in the live process, which returns to the system all the freed
  memory the allocator can. It gave back **29 %** in run A (275 MiB to 195 MiB) and **34 %** in run B (288 MiB to
  191 MiB). About two thirds were not freed memory but memory still in use: objects that something still held.

## What was held: the file descriptors

A second run (P0) counted the process's open file descriptors (`ls /proc/<pid>/fd | wc -l`) at rest after each cycle
of the same size:

| | Descriptors | Resident anonymous memory |
| --- | --- | --- |
| At the start | 7 | |
| After cycle 1 | 607 | |
| After cycle 2 | 1,207 | |
| After cycle 3 | 1,807 | 71 MiB, and growing |

Each cycle added **exactly 600** descriptors, which is 150 players times 4, and none was ever given back. Four
descriptors for each session were still open long after the player had gone. The sockets of a finished session were
never closed because nothing of the session was ever freed.

## The cause

A reference cycle. The hook that runs a session's commands (`/send`, `/glist` and the others) holds a handle to the
session, and the session held the hook in its shared state and never let it go. Each held the other, so neither was
freed when the player left: the view of the world the proxy kept for the player, the hashers, the
writers, a copy of the configuration, and four duplicated sockets stayed in memory for ever. Nothing in the
allocator was involved.

At 4 descriptors for each session, a proxy under this load would have reached the limit of open files (65,536 in the
unit of [OPERATING.md](OPERATING.md), section 5.7) after about 109 such cycles (65,536 / 600), and refused
connections with "Too many open files" long before its memory ran out.

## The fix

#119. The hook is held by the thread that reads the client, and it ends with that thread, so the session no longer
holds it. Nothing of a session outlives it.

The metrics page gained `meridian_sessions_alive`: the sessions made and not freed yet. After a cycle it must be
back to `meridian_players`, which is 0 when nobody is connected. A figure that stays above `meridian_players`
tells of a leak of this kind long before the memory does.

## The check

The run P2: `main` at `91b2f536` (a release build by the CI), the same cycles of 150 players, **13 cycles** in a row.
All 150 players joined in every cycle, none failed. At rest, a minute after each cycle:

| | Descriptors | `meridian_sessions_alive` | Resident anonymous memory |
| --- | --- | --- | --- |
| At the start | 7 | 0 | 0.4 MiB |
| After cycle 1 | 7 | 0 | 17 MiB |
| After cycle 3 | 7 | 0 | 16 MiB |
| After cycle 8 | 7 | 0 | 16 MiB |
| After cycle 13 | 7 | 0 | 17 MiB |

The descriptors come back to the 7 of the start after every cycle, `meridian_sessions_alive` equals `meridian_players` (0 at rest), and the memory is **flat at 16 to
17 MiB from the first cycle on**, where before the fix it rose by about 6 MiB for each cycle.

## What this does not show

- It is one machine and one kind of player: the simulated players of the load tool, in offline mode, going through
  the proxy to synthetic servers. Real clients were not part of these cycles, and neither were switches between
  servers, fallbacks or kicks.
- Thirteen cycles, about 2,000 players, in a run of 16 minutes. A slower leak, of a kind these cycles do not exercise,
  would not show here. The check that stays is the one of
  [OPERATING.md](OPERATING.md), section 8.2, which an operator can do on their own proxy at any time.
