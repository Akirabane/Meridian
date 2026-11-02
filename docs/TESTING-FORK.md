# Testing the seamless switch with the Horizon fork

A step-by-step guide to a second bench, `banc-fork`, whose three servers run the
Horizon fork that can hold a player and hand them over (`feat/held-player`, commit
`df961f5d2`). It is the bench the seamless switch is tested on. The commands are for
Windows PowerShell 5.1 on this machine: run them one line at a time, in a classic
Windows PowerShell window.

[TESTING.md](TESTING.md) is the first bench, on the launcher's Nightly. Read it first:
this page follows the same steps and only says what differs.

## Where things stand

- The bench itself works today: three fork servers, a sidecar for each, the proxy and
  the panel.
- **Switching is built**: the console's `switch <player> <server>` (and `send`), and
  the in-game `/server <name>`, `/hub` and `/lobby`. It has been tested against fake
  servers only; this bench is where it meets real ones for the first time, which is
  what section E is for.

## What it costs

As for the first bench: about 1.5 GB of memory per server when idle, about 4.5 GB in
all, plus a little for the proxy, the sidecars and the panel; a few hundred megabytes
of disk for the worlds. The two benches use the same ports, so **run one at a time**:
stop the first bench (TESTING.md, step F) before starting this one.

## The paths

| What | Where |
| --- | --- |
| Meridian's programs | `D:\disque\dev\meridian-bench-out\bin\` (`meridian.exe`, `meridian-sidecar.exe`, `meridian-panel.exe`; `COMMIT.txt` gives their commit) |
| The fork's programs | `D:\disque\dev\horizon-fork-out\bin-df961f5d2\` (`horizon-server.exe` and the `horizon-client.exe` it starts; `COMMIT.txt` gives their commit). It replaces `bin-d963bd6ef\`, the build of commit `d963bd6ef`, which logs none of the requests below and freezes a player for 10 s instead of 30 s |
| The prepared game files | the Nightly's `.local\horizon-client\26.3`, which the fork accepts (same game version and world data version) |
| This bench, made in step A | `D:\disque\dev\meridian-bench-out\banc-fork\` |

```powershell
$bin = "D:\disque\dev\meridian-bench-out\bin"
$fork = "D:\disque\dev\horizon-fork-out\bin-df961f5d2"
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

## E. Switch, and what to observe

### From the proxy's console

In the panel, open the **proxy**'s console and type:

```
switch <your name> game1
```

The console waits for the switch to end, then prints one line. When it worked:

```
<your name> moved from lobby to game1 in 412 ms (prepare 230 ms, freeze 61 ms, hand over 74 ms, commit 47 ms): 1890 packets, 1210344 bytes (441 chunks, 23 entities)
```

The numbers above are only an example. The first is the whole switch; then come its
four steps (getting the target ready, freezing you on the source, handing your
state over, and changing your client's world), and what was sent to your client.
In the game, the chat says `You are now on game1.`

When it did not work, the console prints the step that failed and why, for example:

```
<your name> could not move from lobby to game1: prepare: ...
```

and the chat says `Could not move you to game1.` followed by the same reason. You
stay where you were.

Then move back with `switch <your name> lobby`, and on to `game2`. The panel's
**Send to game1 / game2 / lobby** buttons, under Tests, do the same with the player
name you type next to them, and show the console's line.

### From the game

Type `/server game1` in the chat. The chat says `Moving you to game1...`, then
`You are now on game1.`, or `Could not move you to game1.` with the step that
failed. `/hub` or `/lobby` take you back to the lobby the same way. The proxy's
console shows the same result line as above, in its log.

### What to observe

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

1. the time, the command (console `switch`, or `/server` in game), from which server
   to which;
2. how long from pressing Enter to the game reacting normally (a guess in seconds is
   fine, or a screen recording), and the console's result line, with its times;
3. for each line of the table above: seamless or not, and what you saw;
4. the exact chat lines;
5. anything else: a stutter, a sound cut, chunks loading in front of you, the hand or
   the held item changing.

### In the servers' logs

Each server writes `logs\latest.log` in its folder (`$banc\lobby`, `$banc\game1`,
`$banc\game2`). For a switch of `Alex` from lobby to game1, look for these lines, in
this order.

On the target (`$banc\game1\logs\latest.log`):

```
player-expect Alex (<uuid>): ok, expected for 10 s
Alex is held: it arrives from another server
player-import Alex (<uuid>): ok, parts ..., placed false, announced true, recentered false
Alex placed: it has arrived from another server
```

The held line comes right after the login line, and is not followed by "joined the
game". `recentered true` instead of `false` means the new place is in another chunk
than the one your view was built around: the chunks around you then come with the
next ticks. If the switch fails before you are placed, the target logs
`Alex was dropped while held: <reason>` instead of the placed line.

On the source (`$banc\lobby\logs\latest.log`):

```
player-release Alex (<uuid>): ok, moved ..., was frozen true
Alex lost connection: moved to another server
```

`was frozen false` means the freeze (30 s at most) ran out before the hand-over
ended. A refused request logs a `WARN` line with `refused:` and the reason. No line
ever shows the player's record. Copy these lines into your notes for the switch.

## F. Collecting the logs for a bug report

Each card's **Console** in the panel keeps the last 2,000 lines of that process's
output. For a report, from the panel page:

1. open the **proxy**'s console, select its text with the mouse from the first line
   to the last, copy, and paste it into a text file;
2. do the same for the sidecar of the source server and of the target server
   (`lobby-sidecar`, `game1-sidecar`, `game2-sidecar`), and for the two servers;
3. add your notes from section E, the times of the switch, and the `player-` and
   `held`/`placed` lines of the two servers' `logs\latest.log`.

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
