# The seamless switch with real clients over a real network

This page records what was seen on 6 November 2026 when real Horizon clients were moved between
two Horizon servers by Meridian's seamless switch (the state of the player is handed over with the
`player-*` requests, the client stays connected; see [STATE-TRANSFER.md](STATE-TRANSFER.md)),
with the proxy and the first server on a rented machine, the second server on another machine
across the Internet, and the clients on that other machine.
Its servers carry the merged transfer module of Horizon (commit `98af9399c`).

**These are the runs of one session with one client build, on one evening. They are not a load
test and they do not replace a long run with several machines. Raw logs are not kept in this
repository; the numbers below were read from them.**

## 1. Setup

| | |
| --- | --- |
| Date | 6 November 2026, 18:13 to 19:50 UTC (all times below are UTC; the clock of the machine of the clients is the reference, the clock of the rented machine is 0.485 s behind it) |
| Proxy and server A (`real`) | A rented virtual server in Warsaw (4 vCores, about 8 GB of memory, Linux x86_64). The proxy at Meridian commit `91b2f536` (the merge of #132), the sidecar of the same commit, server A beside them on the loopback |
| Server B (`real2`) and its sidecar | The Windows PC of the player, in France: Horizon server at the same commit as A (`98af9399c`), the sidecar of the proxy's commit. Both listen on the PC's loopback only |
| Link between B and the proxy | A reverse SSH tunnel opened from the PC (Windows' own OpenSSH client): the proxy reaches B's game port and B's sidecar on two ports of its own loopback. Nothing listens on the PC's network, nothing is opened in the PC's firewall |
| Clients | Three Horizon clients on the PC, `Net_A`, `Net_B` and `Net_C`, offline accounts, the Nightly 2026-11-05 `66f3981` (581 commits older than the servers' build). One window of 1280x720 for the runs that needed an unthrottled window, small minimised windows otherwise |
| Link of the clients | TCP over the Internet from the PC to the rented machine. Round trip PC to rented machine over one SSH session: **44.2 / 44.7 / 45.6 ms** (min, median, max of 10 samples) |
| Horizon servers | Both with `[transfer] enabled = true` and the proxy's Velocity forwarding. A's server build was made by a CI workflow on the commit above (not signed, no `exporters/`, which only `horizon-prepare` uses); B was built on the PC from the same commit |
| Proxy settings | Both servers had `role = "lobby"`, `soft-cap = 20`, balancing on with `tie-break = "order"` (new logins go to `real`); `switch-carry-position = false` until the first try of run 6, `true` after; the other `switch-*` settings at their defaults (freeze limit 8000 ms, commit margin 2000 ms). Difficulty: peaceful on both servers, set at their consoles (see the limits in section 6) |

The first measures (section 3, run 1) were made before B was moved to the PC: both servers were on
the rented machine, the clients on the PC.

### What this layout is, and is not

- The proxy and A are **on the same machine**. Only B is on another machine. A move to B crosses
  the Internet twice: the proxy and the client (PC to the rented machine), and the hand-over
  (rented machine to the PC through the tunnel).
- The tunnel is TCP inside TCP: it adds jitter and waits at the head of the queue that a real network
  between two data centres would not have.
- **The PC carries the clients and server B.** Their CPU, memory and clock are the same.
- A second PC, a laptop on the same LAN, was planned for B and **was not used**.
- The window's frame times are valid **only when the window is not throttled**. A minimised or
  background window of this client runs at 10 frames per second, which is a frame time of about
  100 ms with no move at all (measured on a client at rest). The runs with a small minimised
  window show 33 to 35 ms frames (30 fps); the runs with the visible window, 13 to 35 ms.

## 2. How a move was measured

- **From the client** (`horizon-ctl` on the client's control socket): the state, the inventory
  and the last event number before the move; frame statistics reset; the command; the wait for the
  proxy's chat line `You are now on <server>.`; one and a half seconds for the world to settle;
  then the events since, the raw frame times, the state and the inventory again. The inventory is
  compared as text, field by field for the state.
- **Ticks**: when a player types `/server`, the client receives `Moving you to <server>...` and then
  `You are now on <server>.`; the difference of their game ticks (20 per second) is the duration the
  player sees. A `switch` or `send` from the proxy's console sends only the second line, so no
  duration in ticks exists for those.
- **From the proxy** (its log, read by the operator): for each switch the time of each step, the
  packets, bytes, chunks and entities sent, the hand-over id of the chain
  `expect, export, import, confirm, release`, and the warnings.
- **From the servers**: the console of B (on the PC) was asked for the player's data
  (`data get entity <name> Pos`, `foodSaturationLevel`, `foodLevel`).

## 3. The runs

### Run 1: both servers on the rented machine (before B moved to the PC)

Two trial moves with an empty inventory, then 10 moves with the player given 5 diamonds, 12 oak
logs, 7 levels of experience and a speed effect (`/server real2`, `/server real`, 5 round trips).

| | |
| --- | --- |
| Moves | 12 of 12 reached the target and ended with the proxy's `You are now on` line. 0 loading screen or reconnection screen. No step back |
| Seen by the client (the 10 moves) | 34, 40, 32, 29, 33, 38, 32, 37, 32, 39 ticks: **29 / 33 / 40 ticks (1.45 / 1.65 / 2.00 s)**, min / median / max. The script's own measure, command to proxy line, 1.60 / 1.80 / 2.13 s |
| Proxy, the two trial moves | Total 1587 ms (prepare 1053, freeze 85, hand over 369, commit 78; 1182 packets, 15 915 348 bytes, 429 chunks, 157 entities) and 1878 ms (prepare 1318, freeze 61, hand over 445, commit 52; 1197 packets, 18 458 377 bytes, 473 chunks, 156 entities). The same hand-over id from the first request to the release; nine parts imported (inventory, ender chest, experience, health, effects, game mode, spawn point, recipe book, advancements); no warning |
| Where the time goes | In the **preparation**: the target's world around the player is sent to the client before the freeze (about 16 to 18 MB). The freeze, the time the player cannot act, was 61 to 85 ms |

### Run 2: server B on the PC, 10 round trips (20 moves)

One measured client, standing still, `/server real2` then `/server real`, 6 seconds apart, with
the same objects, experience and effect.

| | To B (the PC) | To A (the rented machine) |
| --- | --- | --- |
| Moves | 10 of 10 | 10 of 10 |
| Seen by the client, ticks, min / median / max | 34 / 36 / 48 (1.70 / 1.80 / 2.40 s) | 30 / 38 / 42 (1.50 / 1.90 / 2.10 s) |
| Loading or reconnection screen | none | none |

The script's measure was 1.64 / 2.03 / 2.48 s for the 20 moves. The longest was the first move to B
(48 ticks), the only one above 42. After every move to B the console of B gave the player's
saturation (20.0) and food level (20).

### Run 3: moves started from the proxy's console (4 moves)

At 25-second intervals, `switch Net_A real2`, `switch Net_A real`, `send Net_A real2`,
`send Net_A real`.

| | |
| --- | --- |
| Moves | 4 of 4, each seen by the client as `You are now on X.` about 2 s after the command, no other event, no screen |
| Proxy | 1659, 1943, 1705 and 1855 ms in all; prepare 1135 to 1299 ms, freeze 69 to 75 ms, hand over 374 to 486 ms, commit 65 to 87 ms. `send` runs the same chain as `switch`; 0 warning, step back or fallback |
| State | Inventory identical, health 20, food 20, saturation 20.0, experience level 7, the speed effect going on: its remaining time dropped by 488 to 495 ticks over each 25-second interval, as time passing |

### Run 4: `drain --move` with three clients (6 moves)

The three clients on `real`; `drain real --move` (all to `real2`), then `drain real2 --move` (all back),
the two commands 60 seconds apart.

| | |
| --- | --- |
| Moves | 6 of 6, no screen, no step back, inventories identical (the experienced client keeps its 5 diamonds, 12 logs, level 7 and speed effect; the two others, fresh players, hold nothing) |
| Order | One at a time, never in parallel: first drain at 19:03:56.7, 19:03:58.8 and 19:04:01.1 (2.1 to 2.3 s apart); second at 19:04:56.8, 19:04:58.6 and 19:05:00.8 (1.8 to 2.2 s apart), as the clients saw it, on the rented machine's clock |
| What the others see | The players that stay on the source read `<name> left the game`, and those on the target `<name> joined the game`, for each move. The moved player reads only `You are now on X.` (see section 7, `announce`) |
| Frame times | 34 to 35 ms for two of the clients; about 100 ms for the third, which is the throttling of a minimised window (section 1), not the move |

### Run 5: a server dies (fallback, not a transfer)

**5a, the engine of B is killed with one client on it** (a client holding 3 diamonds given by B's
console two seconds before): the client reads, 1.2 s later, `You lost the connection to real2. You
were moved to real.` and plays again on `real` with no loading screen. **The 3 diamonds are gone**:
they had never been saved by B, and a fallback moves nobody's state. The inventory on `real` is the
one `real` last saved (empty), experience level 0, health 20, food 20. The engine of B was started
again by Horizon's supervisor 8 s later (`Done` at +8 s), listening again with its sidecar and the
tunnel untouched.

**5b, the engine of A is killed with three clients on it:** kill at 19:20:39.115 on the proxy's clock;
the supervisor saw it at 39.145; **the proxy moved the three clients to `real2` at 39.292, 39.292 and
39.330, 0.18 to 0.22 s after the kill**; the clients displayed the line (the same text as in 5a) about
0.6 s later. No loading or reconnection screen on the two clients that were not touched by a person
(see the limits). A's engine was ready again at 19:20:47, 8 s after the kill, and the proxy offered it
again; the only proxy warning was one `does not answer` from A's sidecar during the restart. The
experienced client kept its inventory: it is the last one B had saved for it, which was the same.

### Run 6: `switch-carry-position`

With `switch-carry-position = true`, the player arrives at the place where it stood, not where the
target last had it.

| | |
| --- | --- |
| First try, after a configuration `reload` | **Not carried, on 10 moves of 10.** The player was put at B's last saved place and, on the way back, at A's old login place. The proxy's log said `the target took the record, all but the position` and nine parts were imported: the setting was not applied (section 7) |
| After a new login of the client | **Carried on 12 moves of 12**, in spectator mode, flying, from (100.5, 120.0, 100.5): the client's position before and after each move 0.00 blocks apart, and B's console read `[100.5d, 120.0d, 100.5d]` after each of the moves to B. Inventory identical, no screen, 33.9 to 35.3 ms frame times. Seen by the client: to B 34 / 36 / 37 ticks (1.70 / 1.80 / 1.85 s), to A 36 / 39 / 40 ticks (1.80 / 1.95 / 2.00 s) |
| In survival, window visible (6 moves) | The player stood on a stone platform built on B at the same coordinates as the ground of A. **Position exact** at (32.5, 91.0, -31.5) before and after, on the ground on arrival at each move, B's console reading the same place, health 20 and food 20 throughout. Frame times of the unthrottled window **12.9 to 22.7 ms**. Seen by the client: 45, 48, 37, 32, 36, 30 ticks (2.25 / 2.40 / 1.85 / 1.60 / 1.80 / 1.50 s) |

Without the platform the player would have arrived at the same coordinates in a different world:
A's and B's worlds are different, and the switch does not change that.

## 4. What the player said

After the six moves in survival, with the window in front of the player and `switch-carry-position`
on, in the player's own words (French):

> « Parfaitement fluide ; aucune coupure, le monde a changé autour de moi parfaitement ; un instant, pas du tout perceptible »

("Perfectly smooth; no cut, the world changed around me perfectly; an instant, not noticeable at
all.") This is one person's account, **without a video**. The numbers above are not a substitute for it
and it is not a substitute for them.

## 5. Fidelity of the state

On every move with objects: the **inventory** was identical (compared as text), **health** and
**food** unchanged, the **experience level** unchanged (7), the **game mode** and the dimension
unchanged, the **speed effect** present with the same level, its time going down by the time that
had passed (it neither restarted nor stopped), and **the position** as in section 3, run 6.

**Saturation, a difference that is not a loss.** A client logs in with a saturation of 5.0 (the
default of a new player) until the server changes it. The servers' own data said otherwise: for two
fresh players, A read 20.0 (food level 20) before any move, and B read 20.0 (food level 20) after
the first move to B; the clients then read 20.0. The 5.0 → 20.0 change that two clients showed at
their first move is therefore the **client's display** catching up, and nothing was lost on the way.
(For the experienced client, B's console later read 9.0 and 10.0 while the client said 9.0 and then
20.0, and 20.0 again afterwards. B was in peaceful difficulty, in which vanilla refills hunger by
itself; that was not checked.)

## 6. What was not done

- **No client on another machine**: all the clients were on the same PC as server B; no player from
  outside, and only three clients.
- **No long run**: each scenario lasted minutes, the whole session about an hour and a half. Memory,
  descriptors and the tunnel's behaviour over hours were not looked at.
- **No failure injected during a transfer** (a server killed between the export and the import, a
  lost reply, a freeze that runs out, a refused import): only moves in calm and a server killed while
  nobody was being moved. The step back after the commit point was not seen.
- **Two moves at once** between the same two servers (two players crossing) were not run; `drain
  --move` moved the players one at a time.
- **One dimension, small inventories**: no nether or end, no full inventory with enchanted items, no
  advancements beyond the defaults, no mods.
- **No network between data centres**: a tunnel from a home line, 45 ms from the proxy.
- **One build of the servers and an older client**: the clients were 581 commits behind the
  servers. Nothing was checked about the clients of other builds.
- **The judgement of what it looks like** is one person, one window.
- **Two other layouts were not run**: the laptop on the LAN as server B, and several real clients
  at once from several homes.

### Limits of the measures

- **A client that a person touched**: on one of the three clients the log shows keyboard and mouse
  input that no script sent (a pause, walking, then a death by a skeleton on B). The cause is not
  known. That client's figures after the first fallback are not used; the moment of its fallback is.
- B's difficulty came back to `easy` once when its engine was restarted (a value set at the console is
  not kept; it was then set in its `server.toml`).
- The proxy's own figures (section 3, runs 1 and 3, run 5b) are those the operator read in its log
  and gave for these moves. The proxy's breakdown was not kept for each of the 20 moves of run 2,
  the 6 of run 4 and the moves of run 6.

## 7. What was found

1. **A reload of `switch-carry-position` only touches sessions opened after it.** A player's session
   keeps the copy of the configuration that was taken at its login and moves the player with it, as
   `/server` does. After the `reload` the log said `settings changed for the next logins`, and a move
   of a session that was already open did not carry the position (10 moves of 10). A new login did. The
   configuration and operating pages should say so.
2. **A move that carries the position and recenters the arrival has a hand-over step about three
   times longer** than one that does not (read in the proxy's log by the operator).
3. **`announce`**: the proxy sends `announce` true both ways (the target tells its players that
   someone joined, the source tells its players that someone left) and has no setting for it. The
   other players therefore read `left the game` and `joined the game` for a moved player. Two settings,
   `switch-announce-arrival` and `switch-announce-departure`, are proposed for this in
   #139.
4. **A fallback is logged with no duration.** The proxy's log says that the server was lost and
   where the players went, but not how long it took; the times above come from comparing clocks.
5. **A fallback moves no state.** What the player had changed since the last time the server saved it
   is lost, and the state on the new server is its own last saved state. This is the fallback's
   definition, and it is visible in run 5a.
6. **The text of a fallback** is `You lost the connection to <server>. You were moved to <server>.`
   when the server vanished without a reason; it is not the translated reason of
   the reason-text fix, which needs a server that says why.
7. **A server's engine killed with no warning is started again by Horizon's supervisor in about 8 s**,
   and Meridian's sidecars, which read the control endpoint again for each request, needed no help.
8. **A `switch` or `send` from the console sends the player only `You are now on X.`**, so the duration
   the player sees can only be read for `/server`.
9. **The client's frame time is only a measure with a window that is not throttled** (section 1).
10. **Preparation is the long part**: the freeze is 60 to 85 ms on the proxy's side and the total
    1.5 to 2.4 s as the client counts it; sending the target's world to the client is what takes the time.
