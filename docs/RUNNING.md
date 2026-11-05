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
auth-mode = "offline"             # a test setting; the default, "online", checks accounts
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

The proxy has gained settings since the run of section 5: a fallback list (`try`),
a player limit (`max-players`), routing by the host name a player typed
(`[[forced-host]]`), the login modes `online`, `offline` and `hybrid` (`auth-mode`,
which replaces the older `online-mode`, still read), and groups of backends with a
route for each kind of player (`[[group]]`, `[[route]]`). `meridian.example.toml`
shows each of them. A configuration needs `forwarding-secret` as soon as it has a
backend, and the same value must be in the backend's `forwarding.secret`.

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
If the server takes the player back to the configuration phase while they play, as the
proxy does to move a player whose server was lost ([OPERATING.md](OPERATING.md), section
5.11), the player acknowledges, goes through the new configuration phase and is back in
the world; the `RESULT` line says how many times in `moves=`.

### Typing commands and reading the answers

The scripted player can also type, and read what the server says. On the command line,
`--script` gives what to type and when, in seconds since the connection:

```
cargo run -p meridian-testkit --example real_run -- run --port 25565     --script "10:/server game1;60:/hub;70:hello everybody"
```

A text that starts with a slash is typed as a command, any other is said in the chat.
Entries are separated by semicolons (so a command cannot hold one), may come in any
order, and may use decimals. `--seconds` defaults to ten seconds past the last entry; if
you give it, every entry must be earlier. With a script the player no longer runs `/list`
by itself. What is due while the server has the player in a configuration phase (for
instance in the middle of a move) waits until the player is in the world again.

When the session ends the program prints, before the `RESULT` line, one line for each
thing typed and each chat line the server sent, in time order, with the time since the
connection:

```
SENT +10.0s /server game1
CHAT +10.0s Moving you to game1...
CHAT +10.3s You are now on game1.
```

An action bar line is left out. `RESULT` then has `said=` with the number of chat lines.
The program does not judge the answers: a refused move, such as `There is no server
called nowhere.`, is a chat line like the others and the exit status is still decided by
how the session ended.

The same is available to Rust code, in `meridian_testkit::live`, for a program that runs
many players (a network simulator) and wants to know whether a command worked:

```rust
use meridian_testkit::{live::{link, session_linked, Options}, trace::Recorder};
use std::time::Duration;

let (handle, link) = link();
let options = Options { host: "127.0.0.1".into(), port: 25565, name: "Bot".into(), seconds: 60 };
let player = std::thread::spawn(move || session_linked(&options, &Recorder::new(), &link));

let mark = handle.journal.mark();
handle.controller.command("/server game1").unwrap();
let moved = handle.journal.wait_for(mark, Duration::from_secs(10), |line| line.contains("now on game1"));
```

- `link()` makes a `Handle` for the caller and a `Link` for `session_linked`, which is
  `session` with the player controlled. Both ends are made for one session.
- `handle.controller` (`Controller`, cloneable, usable from any thread, no async) has
  `command` (a leading slash is dropped) and `say`. Both return at once. What is typed goes
  out in the order given, once the player is in the world. A text of more than 256
  characters is refused, as the game's chat box does, and so is any text once the session
  has ended (`NotSent`).
- `handle.journal` (`Journal`) keeps the last 256 chat lines the server sent to the player
  (the "system chat" packet, which is what the proxy's answers and a server's are), each
  flattened to plain text with its time and whether it was for the action bar. `lines()`
  lists them, `mark()` gives the number of the next one, and `wait_for(since, timeout,
  test)` waits for a chat line numbered `since` or later that satisfies `test`. A
  translated line with no translation shows its key.
- `parse_script`, `start_script` and `transcript` are what `--script` uses: a thread that
  types a script at its times, and the lines printed at the end.

What goes on the wire. A command is the unsigned Chat Command packet with the text
without its slash, and a message is the Chat packet with no signature: the player is in
offline mode and has no chat keys. A command the player types is answered by the proxy
when it is one of its own (`/server`, `/hub`, `/play`, [OPERATING.md](OPERATING.md)),
and passed to the server otherwise. The tests that cover this are in
`crates/meridian-testkit/src/live.rs` (the exact packets, their order, the wait during a
configuration phase, the lines coming back, against a fake server over TCP) and in
`crates/meridian-proxy/src/switching/tests.rs` (a bot that types `/server east` through
the real proxy, is moved from one fake server to another, and reads both answers).

## 5. What was seen

The run below is the one of the relay, with the scripted player. Runs with a real Horizon client,
including one over the Internet to a proxy on a rented server and, on one machine, moves between
three servers and a server killed or kicking a player, are in
[REAL-CLIENT-RUNS.md](REAL-CLIENT-RUNS.md).

This run dates from commit `ced4163`, which is before the proxy had its console,
its routing and the hybrid login. The proxy has changed a great deal since, so
the table below describes that commit, not today's proxy. The tests of the
repository (relay, login, routing) are run in CI on every change; this run has not
been repeated against a real server since.

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
- Typing (`--script`, `Controller`) has been tried only against fake servers and the real
  proxy in front of fakes. Whether a real Horizon accepts the unsigned command and chat
  packets of a player with no chat keys, and what a real server answers, has not been
  seen.
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
