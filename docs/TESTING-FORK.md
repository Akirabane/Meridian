# Testing the seamless switch with the Horizon fork

A step-by-step guide to a second bench, `banc-fork`, whose three servers run the
Horizon fork that can hold a player and hand them over (`feat/held-player`, commit
`d963bd6ef`). It is the bench the seamless switch is tested on. The commands are for
Windows PowerShell 5.1 on this machine: run them one line at a time, in a classic
Windows PowerShell window.

[TESTING.md](TESTING.md) is the first bench, on the launcher's Nightly. Read it first:
this page follows the same steps and only says what differs.

## Where things stand

- The bench itself works today: three fork servers, a sidecar for each, the proxy and
  the panel.
- **The switch command is still to come.** `switch <player> <server>` arrives with the
  proxy's hand-over (the `meridian-handoff` crate is merged; wiring it into the proxy
  and the console is the next pull request). Until then, the proxy's `send` and the
  in-game `/server <name>` answer "transfer not available yet", and section E is what
  to do once `switch` is there.

## What it costs

As for the first bench: about 1.5 GB of memory per server when idle, about 4.5 GB in
all, plus a little for the proxy, the sidecars and the panel; a few hundred megabytes
of disk for the worlds. The two benches use the same ports, so **run one at a time**:
stop the first bench (TESTING.md, step F) before starting this one.

## The paths

| What | Where |
| --- | --- |
| Meridian's programs | `D:\disque\dev\meridian-bench-out\bin\` (`meridian.exe`, `meridian-sidecar.exe`, `meridian-panel.exe`; `COMMIT.txt` gives their commit) |
| The fork's programs | `D:\disque\dev\horizon-fork-out\bin-d963bd6ef\` (`horizon-server.exe` and the `horizon-client.exe` it starts; `COMMIT.txt` gives their commit) |
| The prepared game files | the Nightly's `.local\horizon-client\26.3`, which the fork accepts (same game version and world data version) |
| This bench, made in step A | `D:\disque\dev\meridian-bench-out\banc-fork\` |

```powershell
$bin = "D:\disque\dev\meridian-bench-out\bin"
$fork = "D:\disque\dev\horizon-fork-out\bin-d963bd6ef"
$nightly = "C:\Users\Akirabane\AppData\Roaming\horizon-launcher\versions\nightly-2026-11-01-4aa6854"
$banc = "D:\disque\dev\meridian-bench-out\banc-fork"
```

## A. Make the bench

```powershell
& "$bin\meridian-panel.exe" init --dir $banc --horizon-server "$fork\horizon-server.exe" --meridian "$bin\meridian.exe" --sidecar "$bin\meridian-sidecar.exe" --resources "$nightly\.local\horizon-client\26.3"
```

It writes, in `banc-fork`: `bench.toml` (seven processes), `proxy\meridian.toml` (the
proxy, the three servers and each one's sidecar), `lobby\`, `game1\`, `game2\` (each
with its `server.toml` and `forwarding.secret`), and `secrets\` (a secret of its own
for each sidecar). It copies nothing and writes no EULA. Check it:

```powershell
& "$bin\meridian.exe" --check "$banc\proxy\meridian.toml"
& "$fork\horizon-server.exe" config --dir "$banc\lobby"
```

The first says `valid, 3 backends, default backend "lobby"`. The second lists the
lobby's settings with `server.port = 25601` and the Nightly's game files as
`engine.resources`.

## B. Accept the Minecraft EULA, yourself

As in TESTING.md, after reading the [Minecraft EULA](https://aka.ms/MinecraftEULA), if
you agree:

```powershell
& "$bin\meridian-panel.exe" accept-eula --dir $banc --i-accept-the-minecraft-eula
```

## C. Run the panel

To have the proxy's detailed log in its console, which a bug report needs, set the log
level in the same window first; the processes the panel starts inherit it:

```powershell
$env:MERIDIAN_LOG = "debug"
& "$bin\meridian-panel.exe" "$banc\bench.toml"
```

Open [http://127.0.0.1:8140/](http://127.0.0.1:8140/) and click **Start all**: the
three servers, then the three sidecars once the servers answer, then the proxy. Wait
until the seven cards say `online`. A fork server's console shows
`Velocity player information forwarding is on (forwarding.secret)`.

## D. Join

With the Horizon client, **Multiplayer**, **Direct Connection**, `localhost:25565`.
You land on the lobby. Before switching, make the state easy to compare:

- put a few distinct items in your inventory and the hotbar, and note the slot you hold;
- walk somewhere you will recognise, and note your coordinates (F3);
- if you can, have a second client on the target server, to see whether you appear
  there and how.

## E. Switch, and what to observe (once `switch` is merged)

In the proxy's console in the panel:

```
switch <your name> game1
```

then back with `switch <your name> lobby`, and to `game2`.

A switch is **seamless** when, on your screen:

| Look at | Seamless | Not seamless |
| --- | --- | --- |
| The screen | the game goes on; no loading screen, no "Joining world", no dirt background | any loading or connecting screen, even short |
| The sky and the light | unchanged | a flash, a black frame, the sky blinking or changing colour |
| Your inventory | the same items in the same slots, the same held slot | an item missing, moved or duplicated |
| Your position | where you stood, facing the same way | moved, fallen, or turned |
| The chat | the proxy's message about the switch, and nothing else | a disconnection message, or the join message of a new login |
| The other players | on the target, you appear; on the source, you leave | you appear twice, or not at all |

For each switch, note in a few lines:

1. the time, the command, from which server to which;
2. how long from pressing Enter to the game reacting normally (a guess in seconds is
   fine, or a screen recording);
3. for each line of the table above: seamless or not, and what you saw;
4. the exact chat lines;
5. anything else: a stutter, a sound cut, chunks loading in front of you, the hand or
   the held item changing.

## F. Collecting the logs for a bug report

Each card's **Console** in the panel keeps the last 2,000 lines of that process's
output. For a report, from the panel page:

1. open the **proxy**'s console, select its text with the mouse from the first line
   to the last, copy, and paste it into a text file;
2. do the same for the sidecar of the source server and of the target server
   (`lobby-sidecar`, `game1-sidecar`, `game2-sidecar`), and for the two servers;
3. add your notes from section E and the times of the switch.

The proxy writes its log with the time of each line, so the lines around the time of
the switch are the ones that matter. The consoles only keep what they have shown since
the panel started: copy them before closing the panel. No secret is ever printed in
these consoles.

## G. Stop and clean up

**Stop all** in the panel stops the proxy, then the sidecars, then the servers, which
save their worlds; then Ctrl+C in the panel's window. To remove this bench and its
worlds, once everything is stopped:

```powershell
Remove-Item -Recurse -Force "D:\disque\dev\meridian-bench-out\banc-fork"
```

This removes only `banc-fork`: the first bench, the fork's programs and the Nightly
are not touched.
