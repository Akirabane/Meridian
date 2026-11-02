# Running the proxy in front of a Horizon server

This page says how to run Meridian in front of one Horizon server on one
machine, how to check that it relays a player correctly, and what such a check
showed. The proxy so far is the transparent relay of milestone J1
([DESIGN.md](DESIGN.md), section 11): it logs a player in, connects to a
backend on the player's behalf, and passes everything between them. Switching a
player between servers is not built yet.

## 1. What you need

- A Horizon dedicated server that has its game files prepared (Horizon's own
  documentation, "Host a server"). Meridian carries no game content and needs
  none.
- Horizon's version at or after the Nightly of 2026-11-01. The server must have
  the Velocity-style player information forwarding that Horizon added on
  2026-10-31, because that is how the proxy gives the backend the player's name
  and unique id.
- The Minecraft EULA, accepted by whoever runs the server, in the server's folder.
  Meridian does not do this for you.
- A built `meridian` (see [BUILD.md](BUILD.md)).

## 2. The backend

Run the Horizon server so that only the proxy can reach it, with online mode off
and a forwarding secret. In `server.toml`:

```toml
[server]
port = 25601
bind = "127.0.0.1"        # reachable from this machine only
online_mode = false       # the proxy checks players, not the server
```

Put the secret in a file called `forwarding.secret` in the server's folder, as one
line with no newline at the end. Start the server. Its log must say:

```
Velocity player information forwarding is on (forwarding.secret)
```

A player who connects to this server directly is refused, and so is a login
signed with another secret. That is the point: the backend trusts only the proxy.

## 3. The proxy

Write `meridian.toml` (every key is described in `meridian.example.toml`):

```toml
listen = "127.0.0.1:25565"
motd = "A Meridian proxy"
online-mode = false              # a test setting; the default, true, checks accounts
connect-timeout-ms = 5000
forwarding-secret = "the same text as in forwarding.secret"
default-backend = "main"

[[backends]]
name = "main"
address = "127.0.0.1:25601"
```

Check it, then run it:

```
meridian --check meridian.toml
meridian meridian.toml
```

`MERIDIAN_LOG` sets the level (`error`, `warn`, `info`, `debug`). Ctrl+C stops the
proxy: it closes the sessions and logs `all connections are closed` and `stopped`.
Players join the proxy's address, not the backend's.

## 4. Checking it with a scripted player

The test kit has a quiet player that connects over TCP, logs in offline, follows the
configuration phase, answers keep-alives, pings and the first teleport, runs
`/list` after three seconds, stays for the time you give, and leaves. It sends no
movement. It writes the id and size of every packet it received to a file.

```
cargo run -p meridian-testkit --example real_run -- run --port 25601 --seconds 20 --out direct.txt
cargo run -p meridian-testkit --example real_run -- run --port 25565 --seconds 20 --out proxied.txt
cargo run -p meridian-testkit --example real_run -- compare direct.txt proxied.txt
```

The first line connects straight to a backend that has no forwarding secret, the
second goes through the proxy to the same server once it has one. The last line
compares the configuration phase and the world's first burst, up to the end of the
first chunk batch, packet by packet. It leaves out the packets that depend on
timing or on the mobs that happened to be around: the time of day, pings,
keep-alives and entity packets. It compares ids, not contents. The program prints
a `RESULT` line and exits with 0 only when the player stayed its time and left.

## 5. What was seen

One run on Windows 11, with Horizon's Nightly of 2026-11-01 (commit `4aa6854`) and
a proxy built in the debug profile from commit `ced4163` (the main branch at
`ad66bbe` plus the scripted player), a flat world with a view distance of 4, one
player at a time, everything on `127.0.0.1`. The server was ready about 2.7
seconds after it started.

| Check | What happened |
| --- | --- |
| Direct connection, no forwarding | The player stayed 20 s and left cleanly. It received 2,046 packets (1.25 MB) |
| Through the proxy, forwarding on | The player stayed 20 s and left cleanly. It received 2,324 packets (1.25 MB). The server logged the player under the name the proxy forwarded (the proxy's own log shows the unique id it forwarded); the proxy logged `joined backend` and, at the end, `left backend after 20.1 s` with packet and byte counts for each direction |
| The two sessions compared | Same ids in the same order for the configuration phase and the first burst (77 and 76 packets before filtering). Compared with nothing left out, they differed at a time-of-day packet that one run had and the other had not; other differences were mobs |
| Direct connection to a forwarding server | Refused at login with `This server is behind a proxy: join through the proxy.`; the server logged that the client did not answer the forwarding request |
| Proxy with the wrong secret | The backend refused the login (`the forwarded data is not signed with this server's secret`). The player saw `Cannot reach the server "main" right now. Try again in a moment.` and the proxy logged the backend's refusal. The secret appears in no message |
| The player leaves | Both sides logged the departure; the proxy's counters closed the session |
| The server stops while a player is connected | The player received the server's own disconnect (`multiplayer.disconnect.server_shutdown`) after 6.2 s, and the proxy logged the end of the session |
| Ctrl+C on the proxy with a player connected | The session was closed and the proxy logged `all connections are closed` and `stopped`. The player saw the connection close; the proxy sends it no message |
| The backend is off | The player received `Cannot reach the server ... Try again in a moment.` in 2.1 seconds (the time Windows takes to refuse a connection on this machine, not the 5-second timeout), and the proxy logged a warning with the player, the backend and the system error. The proxy kept running |
| The backend comes back | The next player got in through the same, untouched proxy |

## 6. Limits of this check

- One player at a time, for seconds. Nothing was measured about load, memory or
  scale; the targets for that are in [DESIGN.md](DESIGN.md), section 17.
- The scripted player does not move. A first version that sent a movement packet
  every second was disconnected by the server for an invalid movement; the cause
  was not established, and movement through the proxy is not covered.
- Online mode of the proxy (checking accounts with the session service) was not
  exercised against the real service in this run. The login with a skin was not
  tried either: the player had no profile properties.
- Contents were not compared, only packet ids and counts, and the timing-dependent
  packets were left out of the comparison.
- The proxy sends no message when it stops. A player sees the connection close.
- It was a debug build on one machine, with the backend on the same host.
