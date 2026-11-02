# Test rig for transfers

This page describes how to test, on one Windows machine, that a player moves
between two Horizon servers that run the same map without seeing anything
happen. It is the bench for milestones J3 and J4 of [DESIGN.md](DESIGN.md)
(section 11), and it measures the four phases of a switch described in section 5
(prepare, freeze, hand over, commit).

It covers:

1. the machine and the folders the rig uses;
2. two Horizon servers on two identical copies of one world;
3. the clients that can drive a test, and what each is good for;
4. what "the player sees nothing" means as numbers, and how each number is taken;
5. the scenarios, with what each one costs to run;
6. how to clean up.

Nothing here needs a window: every program runs headless or off screen.

## 1. Machine and folders

The rig assumes Windows 10 or 11 on x86-64, PowerShell, and a Horizon checkout
that builds. It needs the game files prepared from your own copy of Minecraft
26.3, as Horizon's own docs explain (`horizon-prepare`). Those files are read in
place by every process and are never copied into the rig or into this
repository. Nothing from Mojang goes into Meridian.

All run data lives in one folder outside the repository, called the rig root
below. A good default is a folder on the largest drive, for example
`D:\meridian-run`. Its layout:

```
D:\meridian-run\
  golden\          the world as generated once, never run again
  server-a\        backend A: its server.properties, its copy of the world, its logs
  server-b\        backend B: the same, on another port
  proxy\           Meridian's configuration and logs
  client\          the client's home folder (HORIZON_HOME) and working directory
  traces\          packet captures, client event logs, screenshots, one subfolder per run
  pids.txt         the process ids the rig started, one per line
```

Each process runs with its working directory set to its own folder. This
matters for the Horizon client: it writes its worlds and some files relative to
the folder it starts in, not only under `HORIZON_HOME`. A client started from
the repository would leave a `saves` folder there.

Ports, all on `127.0.0.1`:

| Process | Port |
| --- | --- |
| Meridian (what the client joins) | 25565 |
| Backend A | 25601 |
| Backend B | 25602 |
| Horizon client control socket | a file in `client\`, not a port |

Binding everything to `127.0.0.1` keeps the backends unreachable from outside,
which DESIGN.md section 3 requires, and it avoids the Windows firewall prompt that
appears the first time a program listens on all interfaces.

## 2. Two servers, one map

### Build

From the Horizon checkout, in the `fast` profile (optimised, quick to link):

```
cargo build --profile fast -p horizon-client --bin horizon-client
cargo build --profile fast -p horizon-server -p horizon-bots
cargo build --profile fast -p horizon-client-lib --bin horizon-ctl
```

The dedicated server is the game itself run headless:
`horizon-client --resources <prepared> --dedicated --nogui --port N`, started in
the server's folder. `horizon-server run` wraps the same engine in a supervisor
with a panel and crash restarts. The rig starts the engine directly, so that it
owns the process id, reads the console on standard output and writes console
commands on standard input. `horizon-server init` is still the easy way to write
a first `server.properties` and `eula.txt`.

### Make the golden world once

1. Create `server-a` with `horizon-server init --eula --world map --seed 12345
   --port 25601 --offline --no-whitelist --yes`, run there.
2. Edit its `server.properties` (the keys below), start it once, wait for
   `Done (`, and stop it with `stop` on the console. The world is now generated
   and saved.
3. Copy the whole folder to `golden\`. Never start a server in `golden\`.

The keys that must be equal on both backends, and why:

| Key | Value | Why |
| --- | --- | --- |
| `level-name` | `map` | Both servers open their own copy of the same world |
| `level-seed` | `12345` | The proxy checks the seed at prepare time (DESIGN.md section 8) |
| `level-type` | `minecraft\:flat` | Small, fast to load, no terrain differences to explain |
| `online-mode` | `false` | Backends do not authenticate: the proxy does (section 3) |
| `server-ip` | `127.0.0.1` | Never reachable from outside |
| `enforce-secure-profile` | `false` | The first version needs unenforced signed chat (section 7) |
| `view-distance`, `simulation-distance` | `8` | Same view on both sides, so the chunk sets match |
| `gamemode`, `force-gamemode` | `creative`, `true` for flying bots; `survival` for the combat scenario | Bots must not touch blocks unless the scenario says so |
| `difficulty` | `normal` | A difference would be part of the delta |
| `spawn-protection` | `0` | Lets the scenarios place and break blocks near spawn |
| `server-port` | `25601` or `25602` | The only key that differs |

Two backends that run independently will drift apart over time: the clock
advances, the weather changes, mobs spawn and wander. All of that shows up in the
delta, which is correct but noisy. For tests that compare exact states, freeze
the world in the golden copy before saving it: turn off the daylight cycle, the
weather cycle and mob spawning with `/gamerule` (in 26.3 the rules have
snake_case names; check them with tab completion on the console), and remove
existing mobs with `/kill @e[type=!player]`. The combat scenario summons the mobs
it needs.

### Fresh copies before every run

DESIGN.md section 8, option A, wants identical copies that nothing writes back
to. A Horizon server saves its world while it runs and when it stops, so the rig
gives each backend a fresh copy before each run instead of trying to keep a copy
read-only:

```
robocopy D:\meridian-run\golden D:\meridian-run\server-a /MIR /NFL /NDL /NJH /NJS
robocopy D:\meridian-run\golden D:\meridian-run\server-b /MIR /NFL /NDL /NJH /NJS
```

then set `server-port` in `server-b\server.properties` to `25602`.

`/MIR` makes each folder an exact mirror and deletes anything left over from the
last run. A flat world with a view distance of 8 is a few megabytes, so a full
copy takes well under a second. Hard links would be lighter still, but both
servers would then write into the same region files, which is option B and is
ruled out.

### Start and check

Start each backend from its own folder, with `HORIZON_NO_AUDIO=1`, and record its
process id in `pids.txt`. A backend is ready when its console prints
`Done (`. Before any test, check that the two backends really describe the same
world:

- the console's `seed` answers the same number on both;
- a bot that joins each one directly (section 3) receives the same chunk count for
  the same position;
- the configuration digests the proxy computes (DESIGN.md section 3) are equal.

If one of these differs, the run stops there: a switch between them is not
supposed to work.

## 3. Clients

Three kinds of client can drive a test. They answer different questions, and the
milestones use them in this order.

### The test kit's scripted client

`meridian-testkit` (DESIGN.md section 10) has a scripted fake client and fake
backend. It needs no game and no Horizon build, runs in `cargo test`, and is the
first line for J3: it checks that the client's state after a switch equals the
target backend's description, and that no packet from the old backend arrives
after the commit. Everything that can be proven there should be, because it is
fast and deterministic. The rest of this page is about the runs that need real
servers.

### `horizon-bots`, a protocol client

`horizon-bots` is Horizon's load-test client. It speaks the protocol of the 26.3
client without drawing anything: it logs in offline, goes through configuration,
answers keep-alives and pings, accepts position corrections, acknowledges chunk
batches and moves at 20 Hz, deterministically from `--seed`. It costs almost
nothing to run, so it is the client for J3 against real backends and for repeated
runs.

The options that matter here:

- `--server 127.0.0.1:25565 --bots 1` joins through the proxy with one player;
- `--behaviour idle`, `walk`, `stand` or `fight` gives the scenarios of section 5;
- `--name P` names the player `P0`, which the proxy's switch command can target;
- `--capture FILE` records every play packet the first bot receives;
- `--report FILE` writes a summary with chunks received and resent, position
  corrections (`pulled_back`), bytes in and out, and the last health.

The capture file is the main evidence. It holds one record per clientbound play
packet, little-endian: a `u32` of milliseconds since the bot joined, an `i32`
packet id, a `u32` body length, then the body. The packet ids decode to names
with `horizon-protocol`'s `play::Clientbound`, the same crate Meridian pins.
Horizon's `net_replay` example already reads this format and is a model for a
small analyser in Meridian's own tools.

What the bot cannot show: anything drawn, the client's own prediction, or how a
real client reacts to a packet it does not expect. A bot that receives a Respawn
or a return to configuration may simply fail, which is itself a clear failure of
the run.

### The Horizon client, off screen

The Horizon client is the only client that can show what a player sees. It runs
without a window with `--offscreen` and is driven through its control socket with
`horizon-ctl` (documented in Horizon's `docs/horizon-ctl-commands.md`). It is the
client for J4.

```
set HORIZON_HOME=D:\meridian-run\client\home
set HORIZON_NO_AUDIO=1
cd /d D:\meridian-run\client
horizon-ctl --socket D:\meridian-run\client\ctl.sock launch --offscreen --game <horizon-client.exe> --resources <prepared>
horizon-ctl --socket D:\meridian-run\client\ctl.sock connect 127.0.0.1:25565 P0
```

The commands the rig uses:

- `state` gives the player's position, health, food, effects and the open screen
  (`playing` while in the world);
- `events --since SEQ` lists what happened, oldest first, with kinds `screen`,
  `chat`, `death`, `respawn`, `item`, `world` and `dimension`;
- `stats reset` then `stats` gives frame times over a window (average, p95, p99,
  maximum, and the slowest frames);
- `screenshot PATH` writes the next rendered frame as a PNG, also off screen;
- `hold KEY SECONDS` and `look YAW PITCH` move the player as a person would.

A vanilla client is needed for the vanilla-mode half of J4. It cannot be scripted
on this rig, since it is Mojang's program and needs the official launcher, so that
run is done by hand and its findings are written next to the automated ones.

## 4. What "sees nothing" means, as numbers

A switch passes only if every check below passes. Each check names where its
number comes from.

### No new world

After the client's first Login packet, the capture contains none of these
clientbound packets, at any time in the run: `Login`, `Respawn`,
`StartConfiguration`, `Transfer`, `Disconnect`. With the Horizon client, the
`events` log of the run has no `screen` event that opens `level_loading`, and no
`respawn`, `world` or `dimension` event, and `state` reports `playing` before and
after the switch. This is the check that defines the whole project.

### The switch is short

Three durations, all logged per switch:

- **Freeze**: from the moment the proxy stops forwarding to the old backend to
  the moment it sends the delta bundle. Taken from the proxy's own log or metrics.
  Target: 3 ticks (150 ms) or less, as DESIGN.md section 5 aims for.
- **Gap seen by the client**: the longest interval between two clientbound packets
  in the capture during the switch, compared with the longest interval in a
  baseline run without a switch. The backends send keep-alives, time and movement
  regularly, so an idle client normally sees a packet at least every 50 to 100 ms.
  Target: no more than the baseline's longest gap plus the freeze.
- **Frame time** (Horizon client): `stats reset` just before the switch and
  `stats` just after. Target: the maximum frame time is no worse than in the
  baseline run.

### Nothing lost, nothing doubled

- Every serverbound packet the proxy queued during the freeze reaches the new
  backend once. The proxy logs how many it queued and how many it replayed; they
  must match. A scenario that acts during the freeze (a chat line, a block placed)
  checks the result on the new backend: the message appears once, the block is
  there once.
- No packet from the old backend reaches the client after the commit. The proxy
  tags each forwarded packet with its source; the trace must show none from the
  old one after the commit marker.
- The client is never kicked for a keep-alive timeout, and no backend logs one.
- The packets of the switch arrive inside one bundle: a `BundleDelimiter`, the
  delta, then a closing `BundleDelimiter`.

### No jump in the player's state

Compare the client's view just before and just after the switch:

- **Position**: an idle player does not move by more than 0.01 block, and the
  capture has no `PlayerPosition` correction from the new backend. A walking
  player continues on the line it was walking: the distance between where it
  would have been and where it is stays under 0.1 block.
- **Health, food, saturation, experience, game mode, held slot**: equal.
- **Effects**: the same effects with the same amplifiers; remaining time lower by
  no more than the ticks the switch took.
- **Inventory**: the same item in every slot; the total count of every item is
  equal (nothing duplicated, nothing lost).
- **Chunks**: none sent again for the same map. The bot's report counts chunks
  `resent` while it still held them; it must be zero across the switch, or the
  ghost's chunk hashes are not doing their job.
- **Entities**: the changes are only the ones the two servers really disagree on.
  With a frozen golden world, an idle scenario changes no entity except the other
  players.

### No black frame (Horizon client)

Take screenshots at a fixed rate around the switch, for example every 100 ms from
one second before to one second after. No frame may be uniformly dark (a mean
brightness under a tenth of the frames before the switch), and no frame may show
the `level_loading` screen. A small script compares each frame's mean brightness
with the frames before the switch.
