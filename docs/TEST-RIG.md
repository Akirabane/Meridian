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
