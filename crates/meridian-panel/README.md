# meridian-panel

A small local panel to try Meridian on one machine: a test bench of one proxy and
three Horizon servers (lobby, game1, game2), started, stopped, watched and driven
from a page in your browser.

It serves the page on `127.0.0.1` only. It downloads nothing, sends nothing
anywhere, and copies nothing: you give it the programs, it writes configuration
files in the bench folder. It starts only the processes of the bench file and
stops only the ones it started.

## What it costs

A Horizon server takes about 1.5 GB of memory when idle, so a bench of three takes
about 4.5 GB, plus the proxy and the panel, which take little. Each server also
uses processor time while its world is loaded. The panel shows each process's
processor and memory use.

## Setting up a bench

You need a built `meridian` (see [docs/BUILD.md](../../docs/BUILD.md)) and an
installed Horizon release with its game files prepared (Horizon's documentation,
"Host a server"). The panel does not install or copy them.

```
meridian-panel init --dir BENCH --horizon-server PATH/horizon-server --meridian PATH/meridian [--resources PATH]
```

`init` writes into `BENCH`:

- `bench.toml`: the four processes, their folders, ports and how each one stops;
- `proxy/meridian.toml`: the proxy on `127.0.0.1:25565`, offline login (a bench on
  one machine), the three servers as backends, `lobby` as the default server, and a
  random forwarding secret;
- `lobby/`, `game1/`, `game2/`: each with a `server.toml` (on `127.0.0.1`, ports
  25601 to 25603, online mode off, backups off, Horizon's own web panel off so the
  three do not ask for the same port) and a `forwarding.secret` with the proxy's
  secret.

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

"Start all" starts the servers, then the proxy once they answer. "Stop all" stops
the servers, then the proxy. A process is stopped with its console command (`end`
for the proxy, `stop` for a server) and killed if it has not ended after
`stop-timeout-seconds` (30 by default).

The console shows the last 2,000 lines of the process's output and sends a line to
its input: for the proxy `list`, `servers`, `send`, `kick`, `reload`, `end`; for a
server `stop` or any of its commands.

The test buttons ask the proxy for its players (`list`), and to send a player to
one of the servers (`send <player> <server>`), and show its answer. Moving players
is not built in Meridian yet, so for now the proxy answers that the transfer is not
available.

`meridian-panel --check BENCH/bench.toml` checks a bench file without running
anything. Ctrl+C in the panel's terminal ends the panel, and reaches the processes
it started too, which stop as they do on Ctrl+C.

## Safety

- The page and its API listen on `127.0.0.1` only; there is no setting for another
  address.
- Every API call, reads included, needs a token made at each start and written in
  the page. A page from another site cannot read it, and the panel refuses requests
  whose `Host` header is not its own, so a site that points its name at your
  machine cannot use the API either.
- The API takes the name of a process of the bench, a console line, or a player
  name and a server of the bench. It never takes a program or a path.
- The forwarding secret is written in the proxy's and the servers' files only. The
  bench file, the page and the panel's own messages never show it.
- The page sends a content security policy that allows only its own script, and
  puts the processes' output on screen as plain text.
