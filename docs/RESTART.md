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

- Nothing in Meridian listens to the management protocol: the proxy is plain `std` threads
  with no WebSocket and no TLS client, and the sidecar relays only the player requests and
  `status`.
- The panel restarts servers (`Supervisor::restart`) but does not tell the proxy. It is a
  separate process from the proxy.
- Nothing moves players off a draining backend. `drain` only closes the door.

## The signals

| Signal | When it comes | Cost | Status |
|---|---|---|---|
| `server/stopping` (the backend announces) | Before the game stops, with the players still on it | A WebSocket and TLS client, in the sidecar (it is on the backend's machine and can reach `127.0.0.1`) | Parsed (`restart::notice`) and handled (`Detector::handle_notification`); not fed by anything yet |
| The panel restarts it | Before the stop | The panel tells the proxy | `Detector::announce(.., Cause::Panel, ..)`; not fed yet |
| The sidecar does not answer for 3 rounds in a row | After the backend is already down | None: it reads what the poller already knows | Wired |

The first is the one that gives time to move players, so it is the one to feed. The third
is the floor: it needs nothing from the backend, so it works with any server, and it still
marks the backend draining and hands its players to the evacuation, which is late but not
nothing (a player on a backend that hangs rather than stops is still on a live connection).

## The detector

`Detector` (one per proxy, `Proxy::restarts`) keeps, per backend, whether it is going down.

- `announce(backend, cause, now)` marks the backend draining, as the console's `drain`
  does, and calls the evacuation once. A second notice for the same restart does nothing.
- `observe(backend, answered, now)` is called after every round of the poller
  (`restart::observe_round`). Three rounds without an answer (`Settings::misses`) is
  `Cause::Unreachable`.
- A restart ends, and the backend is offered again, when it answers after having gone quiet,
  when `server/started` arrives, or when it answered for a whole `Settings::window` (60 s)
  after the notice without ever going quiet (a restart quicker than a round, or a stop that
  did not happen). A backend answers for a while as it shuts down, so an answer right after
  the notice is not a return.
- Only the marking the detector made is undone. A backend the operator drained before stays
  drained, but its players are still evacuated.

The evacuation is the `Evacuate` trait: `evacuate(backend, cause)`, called with the backend
already draining so the players sent away cannot be sent back to it. Today it is `LogOnly`:
it says what would be moved and the players stay, as they do after `drain`.

## What is left to wire

1. The evacuation: an `Evacuate` that moves the players of the backend to the lobby or to
   another game server, on `drain --move` once it is merged. Replace `LogOnly` in
   `Proxy::new`.
2. `server/stopping`: the sidecar opens the management WebSocket of its backend (it needs
   the backend's `management-server-*` settings and secret) and tells the proxy. That is a
   new message of the sidecar's wire, so it is left to the work that changes the wire. The
   proxy then calls `Detector::handle_notification(backend, line, now)`, or
   `announce(backend, Cause::Announced, now)` if the sidecar sends the fact and not the line.
3. The panel: it tells the proxy before a restart, through the same channel, as
   `Cause::Panel`.
4. The console's `servers` line says "draining (drained from the console)" for a backend the
   detector drained; it should say it is restarting.
5. `Settings` has no keys in the configuration yet; the defaults are 3 rounds and 60 s.

One limit of the marking: the drain is one set, so if the operator drains a backend while
the detector's marking stands, the detector undrains it when the backend is back. A
separate set in the balancer would avoid it; it is left out so as not to touch `balance.rs`.
