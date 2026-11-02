# meridian-sidecar

A small program that runs next to each Horizon server behind a Meridian proxy. It
talks to the server's local control endpoint and offers the proxy the player
requests of a switch (DESIGN.md section 6, decision 8):

| Request | What the backend does |
| --- | --- |
| `expect` | holds the player when they log in through the proxy |
| `export` | gives the player's record, to move to another backend |
| `import` | puts a record on the held player |
| `release moved` / `release abort` | ends a switch: the player has gone for good, or stays |
| `drop` | forgets an expected or held player |

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
