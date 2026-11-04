# Testing Meridian on this machine

A step-by-step guide to try Meridian with three Horizon servers on one Windows 11
machine, with the panel (`meridian-panel`). The commands are for Windows PowerShell
5.1: run them one line at a time, as written.

## What you can test today, and what you cannot

Moving a player from one server to another (`switch`, `send`, and the in-game
`/server <name>`, `/hub` and `/lobby`) is built and tested against fake servers, but
has not been tried on real ones yet. It needs Horizon servers that take players from
another server (the held-player work) and a sidecar next to each server. Without
them it answers which step failed, and the player stays where they are. A test
without them covers:

- joining the proxy with the Horizon client and landing on the lobby;
- the three servers and the proxy online in the panel, with their CPU and memory;
- the proxy's console: `list`, `servers`, `kick`, `reload`;
- the in-game `/server`, which lists the servers.

## What it costs

Each Horizon server takes about 1.5 GB of memory when idle, so the bench takes about
4.5 GB, plus a little for the proxy, the three sidecars and the panel. The three worlds take a few hundred
megabytes of disk once generated. Close what you can spare before starting the bench.

## The paths on this machine

| What | Where |
| --- | --- |
| Meridian's programs | `D:\disque\dev\meridian-bench-out\bin\` (`meridian.exe`, `meridian-sidecar.exe`, `meridian-panel.exe`; `COMMIT.txt` says which commit they come from). `init` takes `meridian-sidecar.exe` from beside `meridian.exe`, or from `--sidecar PATH` |
| The bench, made in step A | `D:\disque\dev\meridian-bench-out\banc\` |
| Horizon's Nightly, installed by the launcher | `C:\Users\Akirabane\AppData\Roaming\horizon-launcher\versions\nightly-2026-11-01-4aa6854\` |
| Its prepared game files (650 MB) | the Nightly's `.local\horizon-client\26.3\` |

The three servers read the game files from the Nightly's folder through
`[engine] resources` in their `server.toml`: nothing is copied. The Nightly's own
`horizon-server.exe config` reads such a `server.toml` without complaint (checked).
The game opens these files to read them; that three servers can share them while
they run is what the first run will confirm.

Open PowerShell and set three names for the rest of the guide:

```powershell
$bin = "D:\disque\dev\meridian-bench-out\bin"
$nightly = "C:\Users\Akirabane\AppData\Roaming\horizon-launcher\versions\nightly-2026-11-01-4aa6854"
$banc = "D:\disque\dev\meridian-bench-out\banc"
```

If you open another PowerShell window later, set them again there.

## A. Make the bench

```powershell
& "$bin\meridian-panel.exe" init --dir $banc --horizon-server "$nightly\horizon-server.exe" --meridian "$bin\meridian.exe" --resources "$nightly\.local\horizon-client\26.3"
```

It writes, in `banc`:

- `bench.toml`: the seven processes for the panel: the proxy, three servers and a
  sidecar for each server;
- `proxy\meridian.toml`: the proxy on `127.0.0.1:25565`, in offline login (nobody's
  account is checked, which suits a bench on one machine), with the three servers
  and the address of each one's sidecar (ports 25580, 25581, 25582);
- `lobby\`, `game1\`, `game2\`: each a `server.toml` (ports 25601, 25602, 25603, on
  `127.0.0.1` only, online mode off, and the transfer module on: `[transfer]
  enabled = true`, which a server that moves players needs and which is off by
  default) and a `forwarding.secret` holding the proxy's secret;
- `secrets\`: `lobby.key`, `game1.key`, `game2.key`, a secret of its own for each
  sidecar, which the proxy reads from the same file. Neither the panel nor `init`
  ever shows them.

It ends by printing the EULA command of step B. You can check what it wrote:

```powershell
& "$bin\meridian.exe" --check "$banc\proxy\meridian.toml"
& "$nightly\horizon-server.exe" config --dir "$banc\lobby"
```

The first says `valid, 3 backends, default backend "lobby"`; the second lists the
lobby's settings, with `server.port = 25601` and `server.online_mode = false`.

## B. Accept the Minecraft EULA, yourself

A Horizon server does not start until the person who runs it has accepted the
[Minecraft EULA](https://aka.ms/MinecraftEULA). The panel never does it for you.
Read it, and if you agree, run:

```powershell
& "$bin\meridian-panel.exe" accept-eula --dir $banc --i-accept-the-minecraft-eula
```

It writes `eula=true` in `lobby`, `game1` and `game2`, and says so for each. Without
it, the panel refuses to start a server and its message gives this command.

## C. Run the panel

```powershell
& "$bin\meridian-panel.exe" "$banc\bench.toml"
```

Leave this window open: the panel runs in it. Open
[http://127.0.0.1:8140/](http://127.0.0.1:8140/) in your browser. The page carries
the access token of this run by itself; there is nothing to type. Opened in another
way, or after the panel restarted, reload the page.

You see seven cards: proxy, lobby, game1, game2, and lobby-sidecar, game1-sidecar,
game2-sidecar, all stopped.

Click **Start all**. The servers start first; each card goes from `starting` to
`online` when its server accepts connections. The first start generates each world,
which takes a while; the panel then starts the three sidecars, and once they answer,
the proxy. Once all seven are online, each card shows its CPU (100 % is one processor
core) and memory, with a curve over the last minutes. A server's figures include the
game process it starts. A sidecar is the small program that will move players in and
out of its server; until switching is built it only waits for the proxy.

Click a card's **Console** to see its output. A server's console shows the line
`Velocity player information forwarding is on (forwarding.secret)`: it trusts only
the proxy.

## D. Join with the Horizon client

Start Horizon from its launcher as usual, then in the game: **Multiplayer**, then
**Direct Connection**, address `localhost:25565`. The server list shows
`Meridian test bench`. From the launcher's command line, the same:

```powershell
horizon-launcher play --server localhost:25565
```

You join with your account's name. The proxy is in offline login, so it checks
nothing and gives you the unique id an offline server would. You land on the lobby.

Joining a server directly, at `localhost:25601`, is refused with `This server is
behind a proxy: join through the proxy.`: that is the forwarding at work.

## E. What to try, and what you should see

In the panel:

| Do | You should see |
| --- | --- |
| **Show players** | `1 player online:` and a line with your name on `lobby` |
| Proxy console: `servers` | each server `answers in N ms`, and `1 player` on lobby |
| Proxy console: `list` | the same as **Show players** |
| A player name, then **Send to game1** | `<name> moved from lobby to game1 in N ms (...)` with the times and bytes, or `<name> could not move from lobby to game1: <step>: <why>`; the player is told either way |
| Lobby console: `say hello` | `hello` in your game's chat |
| Proxy console: `reload` | `reloaded: nothing changed` (edit `proxy\meridian.toml`, for example the `motd`, and `reload` again: it says what changed) |
| Proxy console: `kick <name> testing` | `kicked <name> from lobby: testing`; your game shows the reason `testing` |

In the game, in the chat:

| Type | You should see |
| --- | --- |
| `/server` | `Servers: lobby (default), game1, game2. You are on lobby.` |
| `/server game1` | `Moving you to game1...`, then `You are now on game1.` or `Could not move you to game1.` with the step that failed |
| `/hub` or `/lobby` | `You are already on lobby.` |
| Any other command, such as `/time set day` | It goes to the lobby as usual |

The proxy's own log lines appear in its console marked `[stderr]`: that is where the
proxy writes its log, not an error. Each time the panel checks that the proxy is up,
the proxy logs `Connection closed by the server`; that line is the check, nothing
else.

## F. Stop everything and clean up

1. Leave the game.
2. In the panel, click **Stop all**: the proxy stops first, then the sidecars, then
   the servers, which save their worlds. Wait until the seven cards say `stopped`.
3. Close the panel with Ctrl+C in its window. Ctrl+C also reaches anything the panel
   started, which stops as on its own Ctrl+C; the panel waits for it, then ends what
   is left, the servers' engines (`horizon-client.exe`) included. On Windows this
   also holds when the panel window is closed or the panel is killed. If a server
   then says `another engine still holds the world: pid N`, an engine of an older
   panel outlived it: end that process in the Task Manager and start the server again.

To start again later, run step C again: the bench and its worlds stay.

To remove the bench and its worlds, once everything is stopped:

```powershell
Remove-Item -Recurse -Force "D:\disque\dev\meridian-bench-out\banc"
```

This removes only the bench folder: the Nightly and its game files are not touched.
`D:\disque\dev\meridian-bench-out\bin` can stay for the next test.

## If something goes wrong

| What you see | What it means |
| --- | --- |
| A command prints nothing at all | Some terminals, such as the one built into another application, do not show what a program run with `&` prints. Check its result with `$LASTEXITCODE` (0 is success) and whether the bench folder exists, and use a classic Windows PowerShell window |
| A server card stays `starting` | It has not opened its port yet: look at its console. A first start generates the world |
| `the Minecraft EULA is not accepted` | Step B was not done for that bench |
| `no prepared game files` in a server's console | `--resources` in step A did not point at the Nightly's `.local\horizon-client\26.3`, or the Nightly was removed |
| `Cannot reach the server "lobby" right now` in the game | The lobby is not online: start it, or wait for it |
| `cannot listen on 127.0.0.1:8140` | Another panel is running; close it first |
| The page says the panel does not answer | The panel was closed: run step C again and reload the page |
