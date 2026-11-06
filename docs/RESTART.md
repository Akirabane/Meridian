# Telling that a backend is about to restart

A game server that restarts, on a schedule, after an update or from the panel, takes its
players with it. They are better moved before it goes than left to a broken connection.
This note says what Meridian has for that, what it lacks, and what the detector in
`crates/meridian-proxy/src/restart.rs` adds.

## What exists

- The proxy asks the sidecar of every balanced backend for its state at the probe interval
  (`players`, `capacity`, `accepts_transfers`) and keeps the last answer with its time
  (`status.rs`, docs/LOAD-BALANCING.md section 3).
- A backend that reports `accepts_transfers: false`, or that the operator drains with the
  console's `drain`, is not offered to new players, not even to a named choice. Its players
  stay (`Balancer::drain`).
- A backend whose last answer is older than `state-max-age-seconds` is not believed, and is
  not offered either. That is how a dead backend stops getting players, but only after the
  backend is gone, and nothing moves the players it had.
- Horizon's dedicated server serves the vanilla management protocol: JSON-RPC over a
  WebSocket, set up by the `management-server-*` keys of `server.properties`, TLS on by
  default, authenticated with the `management-server-secret`. It pushes
  `minecraft:notification/server/stopping` to every connected client when the shutdown
  begins (once per run, whether the supervisor or the game's own `stop` began it) and
  `minecraft:notification/server/started` when the game is up again (Horizon's
  docs/server.md, "Vanilla's own management protocol").

## What is missing

- The proxy is plain `std` threads with no WebSocket and no TLS client. The sidecar, which
  runs on the server's machine, is the one that listens (see below).
- The panel restarts servers (`Supervisor::restart`) but does not tell the proxy. It is a
  separate process from the proxy.
- Nothing moves players off a draining backend. `drain` only closes the door.

## The signals

| Signal | When it comes | Cost | Status |
|---|---|---|---|
| `server/stopping` (the backend announces) | When the shutdown begins, with the players still on it; but a Horizon `stop` disconnects them about 78 ms later (measured, [VALIDATION.md](VALIDATION.md), B1), so in that time it drains the server and nothing more | A WebSocket and TLS client, in the sidecar (it is on the backend's machine and can reach `127.0.0.1`) | Fed: the sidecar listens and the proxy asks it (`notices.rs`, "How the announcement reaches the proxy"); off unless set up on both sides |
| The panel restarts it | Before the stop | The panel tells the proxy | `Detector::announce(.., Cause::Panel, ..)`; not fed yet |
| The sidecar does not answer for `silent-rounds` (3) rounds in a row | After the backend is already down | None: it reads what the poller already knows | Wired |

The first was expected to give time to move players. It does not with a plain `stop` of the Horizon build `02f05c1`: the server announces and then drops its players within about 78 ms, and a move takes longer (it reaches the sidecars of both servers before it starts, then freezes and hands the player over). It is still worth feeding: the server is drained at once and no new player is sent to it. To move players ahead of a stop, start the move before the stop: `drain <server> --move`, or the panel's `[drain-before-stop]`. The third
is the floor: it needs nothing from the backend, so it works with any server, and it still
marks the backend draining and hands its players to the evacuation, which is late but not
nothing (a player on a backend that hangs rather than stops is still on a live connection).

## The detector

`Detector` (one per proxy, `Proxy::restarts`) keeps, per backend, whether it is going down.

- `announce(backend, cause, now)` marks the backend draining, as the console's `drain`
  does, and calls the evacuation once. A second notice for the same restart does nothing.
- `observe(backend, answered, now)` is called after every round of the poller
  (`restart::observe_round`), which also puts the `[restart]` table in force, so a reload
  applies to the next round. `silent-rounds` rounds without an answer is
  `Cause::Unreachable`. A round starts every `probe-interval-seconds`, counted from the start
  of the last one (`status::spawn`), so the backend counts as going down between
  (`silent-rounds` − 1) and `silent-rounds` intervals after it stops, plus the time the last
  reading takes to fail: next to nothing on Linux, about 2 s on Windows, where a refused
  connection is tried again, up to `sidecar-timeout-ms` for a machine that does not answer. A
  round that lasts longer than the interval is followed at once by the next.
- A restart ends, and the backend is offered again, when it answers after having gone quiet,
  when `server/started` arrives, or when it answered for a whole `grace-seconds` after the
  notice without ever going quiet (a restart quicker than a round, or a stop that did not
  happen). A backend answers for a while as it shuts down, so an answer right after the
  notice is not a return.
- Only the marking the detector made is undone. A backend the operator drained before, or
  drains while it restarts (`drain` tells the detector), stays drained when the backend is
  back, but its players are still evacuated. An operator who offers the backend again with
  `undrain` is believed: `servers` no longer says it is restarting.
- `servers` says `draining (restarting: it announced its stop)`, or `the panel is restarting
  it`, or `it stopped answering`, for a backend the detector drained.

## The evacuation

The evacuation is the `Evacuate` trait: `evacuate(backend, cause)`, called once for each
restart with the backend already draining, so the players sent away cannot be sent back to
it. The proxy's is `Evacuator`:

- With `[restart] evacuate = false` (the default) it says in the log that the players stay.
  Nothing else changes: the detector's marking is what it was before the table existed.
- With `evacuate = true` it does what `drain <backend> --move` does (`evacuate::send_away`):
  the players on the backend go one at a time, with a pause between two moves, to the servers
  of the same role that the balancer finds, never to the backend itself, never to a server
  that is draining or full, and never to a server for the other kind of player. It runs on a
  thread of its own, so the poller does not wait for the pauses.
- A player with nowhere to go, or whose move failed, stays on the backend. The log has a line
  for each ("`Carol stays on east: no other server of role lobby has room`") and the count
  of those moved and those who stayed.

## How the announcement reaches the proxy

```text
Horizon server ──(management protocol: WebSocket, JSON-RPC)──▶ sidecar ──(signed `notices` request)──▶ proxy ──▶ Detector
```

It is off unless both ends are set up.

- **The sidecar** is started with `--management-properties PATH`, the backend's
  `server.properties`. It reads `management-server-enabled`, `-host`, `-port`, `-secret`
  and `-tls-enabled` from it (again at every connection, since a server that restarts may
  have other settings), connects, and keeps the two notifications that matter
  (`minecraft:notification/server/stopping` and `.../started`, with or without the prefix)
  in a queue of 32. The server has to have its management protocol on
  (`management-server-enabled=true` and a port above 0 in `server.properties`, or in
  `server.toml`'s `[properties]`). Horizon's supervisor serves it, so the connection
  stays up while the game restarts.
- **The proxy** has `sidecar-notices = true` on the backend. It asks the sidecar, over the
  signed exchange, `{"op":"notices","after":N,"wait_ms":2000}`: the sidecar holds the
  request until a notice with a number above `N` comes, or `wait_ms` is over (30 s at most).
  The answer is `{"enabled":…,"watching":…,"last":N,"notices":[{"seq":…,"line":…,"age_ms":…}]}`.
  The first request has no `after` and only learns where the count stands, so what was
  heard before the proxy asked is not replayed. The proxy keeps the number of the last
  notice it took: an answer that is lost loses nothing, and a notice is never taken twice.
  A sidecar that started its count again (it was restarted) is read from the start. A
  notice heard more than two minutes ago is taken but not acted on.
- Each notice goes to `Detector::handle_notification`: `stopping` marks the backend draining
  and calls the evacuation, `started` ends the restart. The proxy only asks the sidecars of
  backends with a role, while `[balancing] enabled` is on, the same ones the poller reads.
  Nothing in `balance.rs` or `placing.rs` is involved.

What the operator is told, once for each change and never at every try: the sidecar cannot be
reached, it does not listen (it was not given `--management-properties`), or it is not
connected to the server's management protocol (it is off on the server, or the server is
down). The secret of the management protocol is in none of these messages, in no error and
in no `Debug` output.

Waiting and bounds:

- After a failure the sidecar waits 1 s before it connects again, then twice as long each
  time, to 30 s, and the wait starts again from 1 s once a connection was made. The proxy
  does the same with a sidecar it cannot reach.
- A WebSocket message is at most 256 KiB; a longer one ends the connection. Fragmented
  messages are put together within that limit, binary ones are dropped, and ping is answered.
  Only the two notifications are kept; the rest of what the server sends is read and thrown
  away.

What is and is not checked:

- **The certificate is not verified.** A Horizon server has TLS on by default and, without a
  keystore, makes a self-signed certificate of its own, so there is no authority to check it
  against. The sidecar therefore accepts any certificate, and for that reason it connects
  only to an address on its own machine: a `management-server-host` that is not `127.0.0.1`,
  `::1` or a name that resolves to them alone is refused, with no connection made. The TLS is
  there so that the sidecar speaks what the server serves; the server is trusted by being on
  the same machine, and the sidecar proves itself to the server by the secret (a bearer
  token, as Horizon takes it).
- The server's `Sec-WebSocket-Accept` is not checked (it needs SHA-1, which nothing else in
  Meridian uses). The upgrade must be answered `101`.
- TLS is rustls on ring, which the workspace already had through the account lookups of
  `meridian-net`: the lock file gains no crate for it.

## What is left to wire

1. The panel: it tells the proxy before a restart, through the same channel, as
   `Cause::Panel`.

With `evacuate = true` and a `stop` typed at the server (B1 of [VALIDATION.md](VALIDATION.md)) the evacuation starts when the announcement is heard, loses the race against the server's own disconnect, and logs `<player> could not move ... the player is already on <x>` and `0 moved, 1 stayed`; the player is then on another server through the fallback (#84), with no cut the client can see. The evacuation of a backend that was drained by the operator before the restart sends its
players away as well, but only with `evacuate = true`; with it off nothing moves them, as
before.
