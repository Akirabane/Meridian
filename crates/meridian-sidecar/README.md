# meridian-sidecar

A small program that runs next to each Horizon server behind a Meridian proxy. It
talks to the server's local control endpoint and offers the proxy the player
requests of a switch (DESIGN.md section 6, decision 8):

| Request | Line on the server's endpoint | What the backend does |
| --- | --- | --- |
| `expect` (`seconds`) | `player-expect <uuid> [seconds]` | holds the player when they log in through the proxy within that time (10 s by default, 60 at most) |
| `export` (`freeze`) | `player-export <uuid> [freeze]` | gives the player's record, to move to another backend; with `freeze`, stops acting on the player's own inputs until a release, and the record carries the `handover` id that starts the hand-over, with `freeze_ticks_left` beside it (asked again, it gives the same record and does not freeze again). Without `freeze` it is a read, with a null `handover` |
| `import` (`record`, `parts`, `announce`, `place`, `arrival`) | `player-import <uuid> {"record":…,"parts":[…],"announce":…,"place":…}`, and with an `arrival` its `"position"`, `"rotation"`, `"velocity"` and `"on_ground"` beside the record | puts the record on the held player, whole or in parts, and lets them into the world; the record must carry the `handover` of its export (a record without one is refused by the sidecar and by the backend); with `place` false the player stays held, and the reply's `placed` says which. An `arrival` (`{"position": [x, y, z], "rotation": [yaw, pitch], "velocity": [vx, vy, vz], "on_ground": bool}`, the last two optional) puts the player there instead of the record's place: the backend reads only the record's player compound. It goes with the `position` part only, and numbers the backend would refuse are refused before it is sent |
| `confirm` (`handover`) | `player-confirm <uuid> <handover>` | places a held player whose record was imported with `place` false (refused when no player is held, its record was not imported, or it is another hand-over's) |
| `release` (`handover`; `moved` with `announce`, or `abort`) | `player-release <uuid> <handover> moved [announce]`, `player-release <uuid> <handover> abort` | ends a switch: the player has gone for good, or stays. The source refuses `moved` once the freeze has run out |
| `drop` (`handover`, optional) | `player-drop <uuid> [handover]` | forgets an expected or held player; after an import with `place` false, cancels the arrival without a word to anyone. After an import it names the record's `handover`, so that a late drop leaves a newer arrival alone |
| `status` | `status` (a placeholder: the Horizon server has not fixed the command) | reports its state; the proxy reads `players`, `capacity` and `accepts_transfers` from the answer, and ignores the rest. Read only, and it names no player. See `docs/LOAD-BALANCING.md`, section 3 |

The lines are those of Horizon's player transfers (its `docs/server.md`, "A proxy moving
a player between two servers"). They are a module of the server that is off by
default (`[transfer] enabled = true`); while it is off, every one of them is answered
`{"error": "transfers are off on this server"}`, and the sidecar passes that on. A request names the player by a unique id of 32
hexadecimal digits, with or without dashes; the sidecar writes it as Horizon does,
lowercase with dashes. It carries a player's record exactly as the backend gave
it: the sidecar never reads it, and the backend that imports it checks it. A
request that cannot become a well-formed line (an id that is not one, a time of
0 or over 60 seconds, `announce` with `abort`, a record that is not a JSON object or
has no `handover` id, an id of the hand-over that is not 32 hexadecimal digits, a
line over the 16 MiB the backend takes) is refused by the sidecar and never
reaches the backend. The backend's answer comes
back as it is: `{"ok": true, ...}`, or a refusal with its `error`.

A hand-over that may still be cancelled imports with `place` false, then either
confirms the player once the proxy has moved its client, or drops it, which leaves
the backend as it was.

A request from the proxy is one JSON object with an `op`, for example:

```json
{"op": "export", "uuid": "123e4567-e89b-12d3-a456-426614174000", "freeze": true}
{"op": "import", "uuid": "123e4567e89b12d3a456426614174000", "record": {...}, "parts": ["inventory"], "announce": false, "place": false}
{"op": "confirm", "uuid": "123e4567-e89b-12d3-a456-426614174000", "handover": "9f2c4be0a1d34e6b8c0f1e2d3c4b5a69"}
{"op": "release", "uuid": "123e4567-e89b-12d3-a456-426614174000", "handover": "9f2c4be0a1d34e6b8c0f1e2d3c4b5a69", "outcome": "moved", "announce": true}
```

When the sidecar cannot get the backend's answer (no answer in time, the endpoint
closed, its file unreadable while the backend restarts), its reply is
`{"ok": false, "error": "...", "unreached": true}`: the backend never said no, and
may or may not have acted. `SidecarClient::recover` makes the connection ready to
ask again after that, or after a connection that broke, by opening a new one; the
proxy asks again only for an export of a frozen player, which the server answers with
the same record each time. A refusal is an answer, and `recover` then does nothing.

Options left out take Horizon's defaults; any other field is refused.

The server needs no network change: it answers these only on its control
endpoint, which listens on 127.0.0.1 with a token.

## Running it

Start the Horizon server with its control endpoint
(`--control-socket <dir>/.horizon/engine.json`), then, on the same machine:

```
MERIDIAN_SIDECAR_SECRET=<the proxy's secret> meridian-sidecar --endpoint-file <dir>/.horizon/engine.json
```

or `--secret-file PATH`. The secret is never an argument, because other users of
the machine can read a process's arguments. It must be at least 16 bytes.

The sidecar listens on `127.0.0.1:25580` unless `--listen` says otherwise. It reads
the endpoint file again for every request, so it keeps working after the server
restarts on another port with another token.

## Telling the proxy that the server is stopping

With `--management-properties <dir>/server.properties` the sidecar also listens to the
server's management protocol (JSON-RPC over a WebSocket, TLS on by default, the
`management-server-secret` as a bearer token) and keeps its `server/stopping` and
`server/started` notifications for the proxy, which asks for them with the `notices` request:
`{"op": "notices", "after": N, "wait_ms": W}` answers the notices numbered above `N` after
waiting up to `W` ms for one. It is off without the option. The server is on this machine:
a `management-server-host` that is not a loopback address is refused, since a certificate the
server made itself is not verified. The secret is in no log and no error. See
[docs/RESTART.md](../../docs/RESTART.md), "How the announcement reaches the proxy".

## Following and changing the server's bans and whitelist

With the same option the sidecar can also carry the lists of the server's management protocol
(`allowlist`, `bans`, `ip_bans`) for the proxy, when its `[access.sync]` is on. Two more
requests:

- `{"op": "access-notices", "after": N, "wait_ms": W}` answers the changes of the lists the
  server announced (`notification/bans/added`, `.../removed`, `ip_bans`, `allowlist`), numbered
  apart from the `notices` above, so that a run of bans never pushes a stop notice out of its
  queue. The answer also says how many connections to the server there have been, and whether
  notices were lost; the proxy reads the lists again when either moves.
- `{"op": "manage", "method": "bans/add", "params": {...}}` makes that call on the connection
  the sidecar already holds and answers its result. Only the three lists and, for each, a read
  and the addition or removal of an entry go through (`allowlist`, `allowlist/add`,
  `allowlist/remove`, `bans`, `bans/add`, `bans/remove`, `ip_bans`, `ip_bans/add`,
  `ip_bans/remove`); nothing else of the protocol, and nothing that clears or sets a whole list.
  A server that does not answer in 10 seconds, or a connection that is down, is `unreached`
  (the proxy may ask again); a server that answers with an error is a refusal.

## What the connection protects, and what it does not

Each connection starts with a random challenge from the sidecar. Every request
and every reply carries a rising number and an HMAC-SHA256 made with the shared
secret over the challenge, the direction, the number and the exact text. So:

- only someone with the secret can make a request or answer one;
- a request recorded and sent again is refused, on its own connection and on any
  other;
- a reply cannot be changed or made up on the way.

What it does **not** do is hide anything. There is no encryption yet: a player's
record (inventory, position, health) crosses the connection as plain text. On the
same machine, over 127.0.0.1, nobody else sees it. Across a network, anyone on
the path can read it.

For that reason the sidecar refuses to listen on an address other machines can
reach. `--allow-remote` alone is still refused while TLS does not exist; adding
`--insecure-remote` accepts it, and every connection from another machine is then
logged with a warning. TLS is the next step for a sidecar on another machine.
