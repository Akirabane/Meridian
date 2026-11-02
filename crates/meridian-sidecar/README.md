# meridian-sidecar

A small program that runs next to each Horizon server behind a Meridian proxy. It
talks to the server's local control endpoint and offers the proxy the player
requests of a switch (DESIGN.md section 6, decision 8):

| Request | Line on the server's endpoint | What the backend does |
| --- | --- | --- |
| `expect` (`seconds`) | `player-expect <uuid> [seconds]` | holds the player when they log in through the proxy within that time (10 s by default, 60 at most) |
| `export` (`freeze`) | `player-export <uuid> [freeze]` | gives the player's record, to move to another backend; with `freeze`, stops acting on the player's own inputs until a release |
| `import` (`record`, `parts`, `announce`, `place`, `arrival`) | `player-import <uuid> {"record":…,"parts":[…],"announce":…,"place":…}`, and with an `arrival` its `"position"`, `"rotation"`, `"velocity"` and `"on_ground"` beside the record | puts the record on the held player, whole or in parts, and lets them into the world; with `place` false the player stays held, and the reply's `placed` says which. An `arrival` (`{"position": [x, y, z], "rotation": [yaw, pitch], "velocity": [vx, vy, vz], "on_ground": bool}`, the last two optional) puts the player there instead of the record's place: the backend reads only the record's player compound. It goes with the `position` part only, and numbers the backend would refuse are refused before it is sent |
| `confirm` | `player-confirm <uuid>` | places a held player whose record was imported with `place` false (refused when no player is held, or its record was not imported) |
| `release` (`moved` with `announce`, or `abort`) | `player-release <uuid> moved [announce]`, `player-release <uuid> abort` | ends a switch: the player has gone for good, or stays |
| `drop` | `player-drop <uuid>` | forgets an expected or held player; after an import with `place` false, cancels the arrival without a word to anyone |

The lines are those of Horizon's held-player work (its `docs/server.md`, "Players
arriving from another server"). A request names the player by a unique id of 32
hexadecimal digits, with or without dashes; the sidecar writes it as Horizon does,
lowercase with dashes. It carries a player's record exactly as the backend gave
it: the sidecar never reads it, and the backend that imports it checks it. A
request that cannot become a well-formed line (an id that is not one, a time of
0 or over 60 seconds, `announce` with `abort`, a record that is not a JSON object)
is refused by the sidecar and never reaches the backend. The backend's answer comes
back as it is: `{"ok": true, ...}`, or a refusal with its `error`.

A hand-over that may still be cancelled imports with `place` false, then either
confirms the player once the proxy has moved its client, or drops it, which leaves
the backend as it was.

A request from the proxy is one JSON object with an `op`, for example:

```json
{"op": "export", "uuid": "123e4567-e89b-12d3-a456-426614174000", "freeze": true}
{"op": "import", "uuid": "123e4567e89b12d3a456426614174000", "record": {...}, "parts": ["inventory"], "announce": false, "place": false}
{"op": "confirm", "uuid": "123e4567-e89b-12d3-a456-426614174000"}
{"op": "release", "uuid": "123e4567-e89b-12d3-a456-426614174000", "outcome": "moved", "announce": true}
```

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
