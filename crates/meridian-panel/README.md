# meridian-panel

A small local panel to try Meridian on one machine: a test bench of one proxy,
three Horizon servers (lobby, game1, game2) and a sidecar for each server, started,
stopped, watched and driven from a page in your browser.

It serves the page on `127.0.0.1` only. It downloads nothing, sends nothing
anywhere, and copies nothing: you give it the programs, it writes configuration
files in the bench folder. It starts only the processes of the bench file and
stops only the ones it started.

## What it costs

A Horizon server takes about 1.5 GB of memory when idle, so a bench of three takes
about 4.5 GB, plus the proxy, the sidecars and the panel, which take little. Each server also
uses processor time while its world is loaded. The panel shows each process's
processor and memory use.

## Setting up a bench

You need a built `meridian` (see [docs/BUILD.md](../../docs/BUILD.md)) and an
installed Horizon release with its game files prepared (Horizon's documentation,
"Host a server"). The panel does not install or copy them.

```
meridian-panel init --dir BENCH --horizon-server PATH/horizon-server --meridian PATH/meridian [--resources PATH] [--sidecar PATH]
```

`init` writes into `BENCH`:

- `bench.toml`: the seven processes, their folders, ports and how each one stops;
- `proxy/meridian.toml`: the proxy on `127.0.0.1:25565`, offline login (a bench on
  one machine), the three servers as backends, `lobby` as the default server, and a
  random forwarding secret, and for each server the address of its sidecar (ports
  25580 to 25582) and the file of the secret the proxy shares with it;
- `lobby/`, `game1/`, `game2/`: each with a `server.toml` (on `127.0.0.1`, ports
  25601 to 25603, online mode off, backups off, Horizon's own web panel off so the
  three do not ask for the same port) and a `forwarding.secret` with the proxy's
  secret.

- `secrets/`: `lobby.key`, `game1.key`, `game2.key`, a random secret of its own for
  each sidecar (32 bytes, in hexadecimal). The proxy and the sidecar read the same
  file; the secret is never written anywhere else, nor shown.

Each sidecar runs in its server's folder with `--listen 127.0.0.1:<port>`,
`--endpoint-file .horizon/engine.json` (where `horizon-server run` puts its control
endpoint) and `--secret-file ../secrets/<name>.key`. `init` takes
`meridian-sidecar` from beside `meridian` unless `--sidecar` names it.

`--resources` names the folder of prepared game files for the three servers; without
it, they look in their usual places. `init` never overwrites a bench.

### The Minecraft EULA

A Horizon server does not start until whoever runs it has accepted the
[Minecraft EULA](https://aka.ms/MinecraftEULA), and the panel never accepts it for
you. Read it, and if you agree, run this yourself:

```
meridian-panel accept-eula --dir BENCH --i-accept-the-minecraft-eula
```

It writes `eula=true` in the three server folders of the bench, and nowhere else.
Without it, starting a server is refused with a message that gives this command.

## Running the panel

```
meridian-panel BENCH/bench.toml
```

Then open `http://127.0.0.1:8140/` (`panel-port` in `bench.toml` changes the port).
The page shows, for each process:

- its state (stopped, starting, online once its port answers, stopping, or exited
  with its code), its process id and how long it has run;
- its processor use (100 % is one core) and memory, with five minutes of history;
- buttons to start, stop and restart it, and to open its console.

"Start all" starts the servers, then the sidecars once the servers answer, then the
proxy once the sidecars answer. "Stop all" stops them in the reverse order: the
proxy, then the sidecars, then the servers. A process is stopped with its console command (`end`
for the proxy, `stop` for a server) and killed if it has not ended after
`stop-timeout-seconds` (30 by default). A sidecar has no console: stopping it ends
it at once.

### Telling the proxy before a server stops

A server that stops takes its players with it, and the proxy goes on offering it to new
ones until it notices. With `[drain-before-stop]` in `bench.toml`, the panel tells the
proxy first. It is off unless the file turns it on:

```toml
[drain-before-stop]
enabled = true
grace-seconds = 5      # how long the panel waits after telling the proxy (5 by default)
move-players = false   # true: also send the players elsewhere (`drain <server> --move`)
proxy = "proxy"        # the proxy to tell; not needed when the bench has only one
```

When you stop or restart a server from the page (or the API), and the proxy is running,
the panel types `drain <server>` (or `drain <server> --move`) on the proxy's console,
shows the server as `draining`, waits the grace, and then stops it as usual. A second
stop during the grace stops it at once. When the server answers again, the panel types
`undrain <server>`, but only if the drain was its own: a server the operator had drained
stays drained. A server that dies by itself cannot be announced, but it is drained at
once so that no new player is sent to it, and offered again when it is back. "Stop all"
does not announce anything, since the proxy stops with the rest; a proxy that is not
running is not waited for.

The proxy knows a server by the name in its own configuration. When that is not the
process's name in `bench.toml`, say so with `backend = "name"` on the server's
`[[process]]`.

The console shows the last 2,000 lines of the process's output and sends a line to
its input: for the proxy `list`, `servers`, `send`, `kick`, `reload`, `end`; for a
server `stop` or any of its commands.

The test buttons ask the proxy for its players (`list`), and to send a player to
one of the servers (`send <player> <server>`), and show its answer. Moving players
is not built in Meridian yet, so for now the proxy answers that the transfer is not
available.

`meridian-panel --check BENCH/bench.toml` checks a bench file without running
anything. Ctrl+C in the panel's terminal ends the panel. It reaches the processes
it started too, which stop as they do on Ctrl+C; the panel waits for them (their
longest stop timeout at most, or until a second Ctrl+C), then ends what is left.

On Windows, each process the panel starts is in a job object of its own, with
everything it starts, such as a server's engine (`horizon-client.exe`). A kill at
the stop timeout ends the whole tree, and so does the system when the panel ends,
however it ends (Ctrl+C, its window closed, killed): no engine is left holding its
world. A server whose world is still held by an engine of an earlier run is not
started; its console says `another engine still holds the world: pid N`, and the
panel leaves that engine alone for you to end.

## Safety

- The page and its API listen on `127.0.0.1` only; there is no setting for another
  address.
- Every API call, reads included, needs a token made at each start and written in
  the page. A page from another site cannot read it, and the panel refuses requests
  whose `Host` header is not its own, so a site that points its name at your
  machine cannot use the API either.
- The API takes the name of a process of the bench, a console line, or a player
  name and a server of the bench. It never takes a program or a path.
- The forwarding secret is written in the proxy's and the servers' files only, and
  each sidecar's secret in its own file under `secrets/`. The bench file names those
  files, never what they hold, and the page and the panel's own messages never show
  a secret.
- The page sends a content security policy that allows only its own script, and
  puts the processes' output on screen as plain text.
