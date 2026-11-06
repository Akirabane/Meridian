# Extensions

This page specifies how a program of the operator's own can take part in what the proxy
decides: whether a player may enter, whether they may use a proxy command, and which server
they go to. Nothing on this page is built yet. It is the plan that the pull requests of
section 10 follow.

**Status: decided.** The operator settled its choices on 2026-11-05 (section 10.3). Horizon's
maintainer then agreed to the form on 2026-11-06: an external program on a local socket, speaking
JSON-RPC with `rpc.discover` as the management protocol does (comment 6679084547 on Horizon's
issue #7484). The questions asked of Horizon are in section 10.4, with what was answered.

Every statement about the code as it is today names a file and a line, checked by searching the
code at `e2211c5` (main after pull request #98). Statements about Horizon name its files on its
main branch at `85d8e283f`.

This page replaces the recommendation of [DESIGN.md](DESIGN.md) section 13.5
(`DESIGN.md:716-721`), which proposed a Rust trait first and a socket later. The operator decided
on 2026-11-05 to build only the socket form, and in Rust first: the reasons are in section 2.

## 1. Purpose, non-goals and words

### 1.1 Purpose

An operator who already has a system of their own for ranks, a store or a matchmaker should be
able to plug it into the proxy without changing the proxy. The proxy asks that system at three
points where it decides alone today, and the system can ask the proxy to act (move a player,
send a message), as the console does.

### 1.2 The scope rule

The proxy owns only what no single server can do on its own:

- routing a player to a server, and moving them between servers;
- falling back when a server dies;
- network-wide accounts and counts (the player limit, the queue, the balancer);
- the commands that cross servers: `/send`, `/glist`, `/alert`.

What a server already owns stays on the servers: operators, a server's own permissions, its
bans and allowlist, chat. **The proxy does not grow a LuckPerms, a GroupManager or an
Essentials.** An extension is the way to bring such a system to the proxy's decisions. It is not
the start of a plugin ecosystem inside the proxy.

### 1.3 Non-goals

- **No module loaded into the proxy.** The workspace forbids unsafe code (`Cargo.toml:37`,
  `unsafe_code = "forbid"`), Rust has no stable binary interface for plugins, and a module that
  crashes takes the proxy and every player with it.
- **No SDK in other languages.** The protocol is documented and nothing on the wire depends on
  Rust, so anyone can write a client. But Meridian ships, tests and supports only the Rust crate
  `meridian-extension` and its examples.
- **No packet access.** An extension never sees or changes the packets of a session. The
  points are a login, a command and a placement, not every packet.
- **No change to the built-in modules.** The bans and whitelist (pull request #81), the
  permissions (#82), the queue (#83) and the fallback (#84) stay as they are, can be turned off,
  and do not grow (section 9).
- **Horizon only.** As for the rest of Meridian, the servers behind the proxy are Horizon
  servers.
- **Nothing changes by default.** A proxy with no `[[extension]]` in its configuration behaves
  byte for byte as it does today.

### 1.4 Words

| Word | Meaning |
| --- | --- |
| Extension | A separate program, written by the operator or a third party, that the proxy connects to. |
| Decision point | A place in the proxy's code where it asks an extension before it acts, and waits for the answer up to a deadline. |
| Order | A request an extension sends to the proxy: list the players, move one, tell one. |
| Event | A message the proxy sends to an extension and expects no answer to (version 1.1). |
| Capability | The right to one decision point, one group of orders or the events, declared by the extension and granted by the operator in the configuration. |
| On-error policy | What a decision point does when the extension does not answer in time, answers something invalid, or is not connected: `default`, `allow` or `deny` (section 4.1). |
| Breaker | The switch that stops asking an extension that keeps failing, for a while (section 4.1). |

## 2. Transport

### 2.1 JSON-RPC 2.0 over a WebSocket on the loopback

The proxy and an extension talk with JSON-RPC 2.0 messages in WebSocket text frames, over TCP
on a loopback address. This is the shape of Horizon's management protocol, which is vanilla's:
JSON-RPC 2.0 over a WebSocket, with namespaced methods such as
`minecraft:notification/server/stopping`, its error codes (`-32700`, `-32600`, `-32601`,
`-32602`, `-32603`) and `rpc.discover` (Horizon `crates/horizon-server/src/management.rs:1-6`,
`:478-482`, `:599-606`). Meridian's sidecar is already a client of that protocol
(`crates/meridian-sidecar/src/management.rs:4-14`), over a small WebSocket code of its own, now the
`meridian-websocket` crate (`crates/meridian-websocket/src/lib.rs`, head comment).

**Who connects.** The proxy connects to the extension. The extension listens on a loopback
port, and the proxy is a client of it, as the sidecar is a client of a server's management
protocol. The reasons:

- The proxy opens no new listening port. A proxy's only listener stays the one players join.
- The configuration says which extensions exist. Nothing can join the proxy as an extension
  because it found a port and a token.
- The lifecycle is one Meridian already has: connect, and when the connection ends, connect
  again after a wait that doubles up to a ceiling (`crates/meridian-sidecar/src/management.rs:434-464`,
  `run`, with `FIRST_WAIT` and `LONGEST_WAIT` at `:44-45`).
- It is what the operator chose: "an external program that Meridian calls".

The other direction (the proxy listens and extensions connect, as tools connect to Horizon's
management protocol) was set aside: the operator chose this one (decision U1, section 10.3).

**Loopback only.** The extension's address must resolve only to loopback addresses (127.0.0.1,
::1). Any other address is refused when the configuration is read, by the rule the sidecar
already applies to a server's management address
(`crates/meridian-sidecar/src/management.rs:269-290`, `Settings::addresses`). An extension on another machine is a
later question, and would need TLS with a checked certificate. The sidecar does not check the
certificate today (`crates/meridian-sidecar/src/tls.rs`).

**One connection for each extension**, carrying both directions: the proxy's requests
(decisions) and the extension's requests (orders). JSON-RPC ids are per direction: the proxy's
are numbers, and an extension's may be numbers or text (a program in another language often takes a
UUID), which the answer carries exactly as it came. A request with no id, or a null one, is a
notification. A message is at most 256 KiB. That limit is the extension protocol's own: the sidecar's reader has one limit
today, 4 MiB, raised so that it can read a server's ban list
(`crates/meridian-websocket/src/lib.rs`, `MAX_MESSAGE`), and the shared code takes the
limit from its caller (section 2.5).

### 2.2 Authentication

Horizon's management protocol takes the secret as a bearer token in the upgrade request, or in
the `Sec-WebSocket-Protocol` header after `minecraft-v1,` from an allowed origin
(Horizon `crates/horizon-server/src/management.rs:417-445`). A bearer token suits a client that
proves itself to a server. Here the proxy is the client, and the token would go to whatever
process holds the port. A local program that started first on that port would learn it.
So the token never crosses the wire:

1. The proxy opens the WebSocket with `Sec-WebSocket-Protocol: meridian-extension` and no
   secret. The extension answers `101` with that subprotocol: an answer that names none is
   refused by the proxy, since the program that answered is not one that speaks this protocol. The
   extension refuses (`403`) an upgrade that carries an `Origin` header, which a browser sends: the
   proofs would keep a page open on the same machine out anyway, and this keeps it from holding
   the connection.
2. The proxy's first message is `meridian:hello` with `proxy_nonce` (32 random bytes, hex).
3. The extension answers with its own `extension_nonce` and
   `proof = HMAC-SHA256(token, "meridian-extension/extension" | proxy_nonce | extension_nonce)`.
4. The proxy checks the proof in constant time. If it is wrong, the proxy closes the connection
   with the WebSocket close code 1008 and no reason, and no JSON-RPC answer, and logs
   `extension "<name>": refused: wrong token`. It retries only after the longest wait, and never
   says why to the other side. The extension does the same when the proxy's proof is wrong. A
   message over the limit closes the connection with the code 1009.
5. The proxy sends `meridian:hello/confirm` with
   `proof = HMAC-SHA256(token, "meridian-extension/proxy" | extension_nonce | proxy_nonce)`.
   The extension checks it the same way. Neither side sends player data, and the extension
   sends no order, before both proofs are checked.

**What the proof is computed over.** The `|` is a concatenation of bytes: the ASCII text of the
label, then the **32 bytes each nonce stands for**, not the 64 characters of their hexadecimal. The
nonces are written in lower case and read in either case, so the bytes are the same whatever case a
side wrote. The two labels differ and the nonces change places, so a proof of one side is never the
other's. A program in another language decodes the hexadecimal before it computes. This vector
is in the code's tests (`crates/meridian-extension/src/handshake.rs`), and was computed apart from
it, with Python's `hmac` and `hashlib`:

```text
token            "0123456789abcdef0123456789abcdef"                      (32 bytes of ASCII)
proxy_nonce      00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff
extension_nonce  ffeeddccbbaa99887766554433221100ffeeddccbbaa99887766554433221100

the extension's proof is over these 92 bytes (28 of label, 32 of proxy_nonce, 32 of extension_nonce):
  6d6572696469616e2d657874656e73696f6e2f657874656e73696f6e
  00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff
  ffeeddccbbaa99887766554433221100ffeeddccbbaa99887766554433221100
proof            546198eb05cd02f2d5802912b4e6d9a00d173fdff4a1fd1a33434e875571bffd

the proxy's proof is over these 88 bytes (24 of label, 32 of extension_nonce, 32 of proxy_nonce):
  6d6572696469616e2d657874656e73696f6e2f70726f7879
  ffeeddccbbaa99887766554433221100ffeeddccbbaa99887766554433221100
  00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff
proof            3719587fddecaa8438f006341b2ae8c64e1bd94b1351f929e5da3870f6e1a226
```

For a check that the two sides agree on bytes: the same extension proof computed over the
hexadecimal text of the nonces would be `4708a5d59de31016000e2141701f01aa5116188506bed5d93be9c32d17db1e5d`,
which is not a valid proof.

The token is a file the operator writes (`token-file`), read again at each connection as the
sidecar reads its server's settings (`crates/meridian-sidecar/src/management.rs:18-19`). It is at
least 32 bytes, with the line end an editor leaves after it taken off. It is held in a type of its
own, `Secret` of `meridian-extension`, as the sidecar's is (`management.rs:198-216`): `Debug` hides
it, there is no `Display`, and it is in no log line and no error. Meridian already computed
HMAC-SHA256 for player information forwarding (`crates/meridian-net/src/backend/forwarding.rs`).
That function now lives in `meridian-hmac`, where both uses reach it.

### 2.3 Capabilities

An extension declares in its `hello` what it wants. The operator grants in the configuration
what it may have. The proxy uses the intersection and logs it once at each connection.
Something declared but not granted is logged as refused. Something granted but not declared is
not used.

| Capability | What it gives |
| --- | --- |
| `decide.login` | The admission decision point (section 4.2) |
| `decide.command` | The permission decision point for the proxy's commands (section 4.3) |
| `commands` | Commands of the extension's own, answered by it (section 4.3) |
| `decide.server` | The choice of server (section 4.4) |
| `orders.read` | Lists of players, servers and the queue (section 5) |
| `orders.message` | Telling a player, or many, a line (section 5) |
| `orders.move` | Moving players (section 5) |
| `orders.moderate` | Kicking a player (section 5) |
| `orders.operate` | Drain, undrain, reload (section 5) |
| `orders.console` | Any console line, as the operator types it (section 5) |
| `events` | The events (section 6, version 1.1) |
| `addresses` | Players' IP addresses in what the extension receives (section 7) |

Each decision point is granted to **one extension at most** in version 1. A configuration that
grants the same point to two is refused when it is read. There are no chains of extensions in
version 1 (decision U3, section 10.3).

### 2.4 The alternatives set aside

- **A named pipe on Windows and a Unix socket elsewhere.** The filesystem protects them, which is
  a real advantage. But they are two implementations with two sets of tests. The standard library
  has no named pipes, so they would need a crate built on unsafe code on the Windows side. And a
  tool to look at the traffic is harder to find. Meridian's CI runs on Linux and Windows
  (`.github/workflows/ci.yml:19`), and one TCP path is the same on both.
- **JSON lines over TCP.** The simplest framing. But it would invent its own handshake and its
  own error shapes where Horizon's management protocol has settled them, and the sidecar already
  reads WebSocket. It stays the fallback if a WebSocket server in the SDK costs too much
  (section 10, risks).
- **HTTP requests (or gRPC) from the proxy to the extension.** No standing connection, so no
  orders from the extension without a second channel, and gRPC brings an async runtime the
  proxy does not use (its model is a thread per connection: `DESIGN.md:976-990`).

### 2.5 The WebSocket code

The proxy's WebSocket code is the sidecar's, extended and moved into a crate that the proxy, the
sidecar and the SDK use, `meridian-websocket`, with no new dependency (decision U6, section 10.3).

The sidecar's code is a client. It reads text messages, whole or in fragments, answers pings,
and since the synchronisation of bans it also sends masked text frames
(`crates/meridian-websocket/src/lib.rs`, `Socket::send_text`). It did not check the server's
`Sec-WebSocket-Accept`, and the sidecar's connection still does not: the servers it reads are on
its own machine, and its tests answer without the header. The proxy is the client of an
extension, so it uses that code nearly as it is. What the shared crate adds:

- the limit of a message as a parameter, 4 MiB for the sidecar and 256 KiB for an extension;
- the server side, for the SDK: reading the upgrade request, answering `101` with the
  `Sec-WebSocket-Accept` a client expects, reading masked frames and sending unmasked ones;
- the check of `Sec-WebSocket-Accept` on the client side (`Socket::open_checked`), now that a
  server of Meridian's own answers it. The proxy's connection to an extension uses it. It also
  wants the subprotocol the request offered to be the one the answer names;
- a socket in two halves (`Socket::split`): a reader on one thread, and a writer that is cloned
  for the threads that send, all writes under one lock so that no two frames are mixed. The
  proxy's threads ask decisions while the reader takes the answers (section 4.1);
- close codes (`close`): `Socket::close` and `Writer::close` send one, and `close_code` returns the
  code the other end closed with. The protocol uses 1000, 1008 for a wrong proof and 1009 for a
  message over the limit.

`Sec-WebSocket-Accept` is the base64 of a SHA-1. `meridian-websocket` names `sha1` and `base64`
as direct dependencies rather than writing them again by hand: both are already in the lock file
(`sha1` through `horizon-protocol`, `base64` through `ureq`), so no crate is added to it.

## 3. Versions

- `meridian:hello` from the proxy carries `protocol: [0]`, the versions the proxy speaks, and
  `proxy: "<version of Meridian>"`. The extension answers with the one it chose, `protocol: 0`.
  If there is none in common, the extension answers the JSON-RPC error `-32600` with
  `data: {"supported": [...]}`, the proxy logs both lists and waits the longest wait before it
  tries again.
- **Version 0 is experimental.** Anything in it may change between two releases of Meridian.
  A proxy with an extension of version 0 logs at start
  `extension "<name>": protocol 0 is experimental`.
- **From version 1, the rule is N and N-1.** A release of Meridian speaks the newest version and
  the one before it. A version is dropped only after a release that has a newer one. A field
  added inside a version is optional, and both sides ignore fields they do not know. A field's
  meaning never changes within a version.
- The SDK's version follows the protocol's: `meridian-extension` 0.x speaks protocol 0.
- Both sides answer `rpc.discover` with the methods they serve, as Horizon's management protocol
  does (Horizon `management.rs:599-606`, `:639`). The method is `rpc.discover`, with no
  parameters (omitted, or an empty array); `meridian:rpc.discover` is the same call. It is
  answered at any time, before the hello too, so that a tool can list what a program serves
  without opening a session. The result is an object with a `methods` array, as Horizon's
  (`management.rs:711-727`; Horizon adds `openrpc`, `info` and `components`, which this protocol may
  send and a reader ignores): each item has the `name` of the method, with its `meridian:` prefix,
  and `params`, a list of `{ "name", "required", "schema" }` (empty or left out when the method
  has none). `rpc.discover` itself is not listed. The proxy lists the orders its version and the
  granted capabilities allow; an extension lists the decision points it serves.

  ```json
  {"jsonrpc": "2.0", "id": 1, "result": {"methods": [
    {"name": "meridian:decide/login", "params": [{"name": "request", "required": true, "schema": {"type": "object"}}]}
  ]}}
  ```

## 4. The decision points of version 1

### 4.1 What all three share

**The call.** Each point is a JSON-RPC request from the proxy, sent on the thread that is
already handling the player: the connection's own thread. No thread is added for a decision,
as the queue adds none for a wait (`docs/OPERATING.md`, section 5.10). One reader thread for
each extension's connection takes the answers and wakes the thread that waits for each, by its
id. The thread model is two threads for each player (`DESIGN.md:976-983`). A slow extension
therefore holds only the players whose decision is waiting, and never another extension or the
proxy's console.

**The deadline.** Each point has a `timeout-ms` in the configuration, with the defaults below.
The deadline counts from the request's send to the answer's arrival. An answer that comes later
is dropped and counted.

**The on-error policy.** For each point, the operator chooses what happens when there is no
usable answer:

- `default`: as if no extension were configured (the built-in modules decide);
- `allow`: let the player in, or let the command run;
- `deny`: refuse the login or the command.

"No usable answer" covers all of these: the deadline passed, the answer is invalid, the
extension is not connected, the breaker is open, or there is no free place for one more request.
For the choice of server, `allow` and `deny` read as `default` and `refuse`. The default policy
is `default` everywhere, so an extension that dies leaves the proxy as it would be without it.

**Concurrency.** Each extension has `max-in-flight` (64 by default): the most requests of the
proxy that may wait for its answers at once. A decision that finds no place does not wait. It
takes the on-error policy at once and is counted as `busy`. This caps the threads that can be
held by one extension at `max-in-flight`.

**The breaker.** After `breaker-failures` decisions in a row that end without a usable answer
(5 by default), the extension's breaker opens for `breaker-seconds` (30 by default). While it is
open, every decision takes the on-error policy at once without a request. Then one decision is
sent: an answer closes the breaker, and a failure opens it again for the same time. The change is
logged once each way (`extension "perms": not answering, decisions use their on-error policy for
30 s`). A broken connection opens the breaker at once, and it is closed again by the next hello.

**Validation.** An answer is usable only when it is valid:

- The required fields are present and of their type. A field the proxy does not know is ignored.
- A string shown to a player (a refusal's reason, a line) is plain text of at most 256
  characters, sent as a literal (`text::literal`, as `refuse` does today:
  `crates/meridian-proxy/src/session.rs:864-867`). It is never read as a JSON text component,
  so an extension cannot put a click event in a player's chat.
- A server it names is one the proxy offered.

An invalid answer is logged with the method, the field and why. It is never logged whole. It
counts toward the breaker.

**The configuration** (one `[[extension]]` for each program; none by default):

```toml
[[extension]]
name = "perms"                       # in logs, metrics and node names
enabled = true                       # false: read and checked, not connected to
address = "127.0.0.1:25590"          # loopback only
token-file = "extensions/perms.token"
grant = ["decide.command", "commands", "orders.read", "orders.message"]
max-in-flight = 64
breaker-failures = 5
breaker-seconds = 30

[extension.decide-login]             # only if granted
timeout-ms = 1000
on-error = "default"                 # default | allow | deny

[extension.decide-command]
timeout-ms = 250
on-error = "default"
cache-seconds-max = 300

[extension.decide-server]
timeout-ms = 1000
on-error = "default"                 # default | refuse
```

The configuration crate checks the address without asking a name server: an IP address on the loopback
or `localhost`, and any other host is refused. The proxy resolves it again at each connection and
refuses an address that is not on the loopback. [CONFIG.md](CONFIG.md) lists every key with its checks,
including the time the two decisions of a login add to the queue's.

It sits beside the other sections of the configuration crate (`crates/meridian-config/src/lib.rs:197-235`
lists `commands`, `balancing`, `restart`, `fallback`, `proxy_protocol`, `metrics`, `access`,
`permissions` and `queue`), in a
file of its own as those have (`meridian-config/src/queue.rs`, `fallback.rs`). It is read at the
start and at `reload`. A reload that changes an extension closes its connection and opens the
new one. The decisions under way keep their old deadline.

### 4.2 Admission at login

**What it decides.** Whether a player who has logged in to the proxy may go on. The extension
can only add refusals: it never lets in a player the built-in modules turn away (see the order
below).

**Where.** `serve` in `crates/meridian-proxy/src/session.rs:478-524`, between the bans and
whitelist (`:487-498`) and the check for a second login (`:499-521`).

**Request** `meridian:decide/login`:

```json
{
  "player": {"name": "Alex", "uuid": "0a2ea4ba-bced-39d2-80e1-1ad51a0e925c", "auth": "verified"},
  "host": "play.example.org",
  "address": "203.0.113.7",
  "connected": false
}
```

- `auth`: `verified` (checked with the session service) or `unverified` (offline mode, or the
  unverified players of the hybrid mode). In the second case **the name and the id prove
  nothing**: the extension is told so in the field, as the operator is in
  `docs/OPERATING.md` section 5.8.
- `host`: the host name the player typed, when the handshake gave one.
- `address`: only with the `addresses` capability; otherwise left out.
- `connected`: whether a session of the same player is open now (a second login).

**Answer:**

```json
{"decision": "allow"}
{"decision": "deny", "reason": "The event starts at 20:00."}
{"decision": "default"}
```

`allow` and `default` both let the player go on to the built-in checks that follow. They
differ only in the metrics and the log, where `allow` records that the extension looked.
`deny` refuses the login with the reason. With no reason, the player reads `You cannot join
this server right now.`. The refusal goes through `refuse` (`session.rs:864`), is logged as
the bans' refusals are (`session.rs:494`), and counts as
`meridian_login_failures_total{reason="extension"}`.

**Default without an extension:** nothing is asked. **Timeout:** 1000 ms.

**The order of the checks, and why:**

1. The address ban and the connection limit, before a byte is read (`crates/meridian-proxy/src/serve.rs:87`).
2. The login itself: the account check in online mode.
3. The built-in bans and whitelist (`session.rs:489`).
4. **The extension's admission.**
5. The second login: refused, or replacing the open session (`session.rs:499-521`).
6. The player limit and the queue (`place`, `session.rs:540-564`).
7. The choice of server (section 4.4).

The extension comes after the built-in lists because they are local, instant and the operator's
own: a ban must hold whatever an extension says, and a banned player costs no request. It comes
before the second login, so that a login the extension refuses does not end the session it would
have replaced. It comes before the queue, so that a refused player never waits, which is the rule
the queue already keeps ("A refused player never waits": `docs/OPERATING.md` section 5.10).

**The time budget of a login.** The proxy sends the client nothing while it decides, and a
client gives up after 30 s of silence, as vanilla's does (`crates/meridian-net/src/client.rs:58`,
`READ_TIMEOUT`). The queue already refuses a configuration whose wait and connect timeout add up
to more than 27 s (`crates/meridian-config/src/queue.rs:12`, checked at `:57-80`). That check
must add the
admission's and the server choice's `timeout-ms`, or a slow extension and a full proxy together
would let the client time out.

### 4.3 The permission of a command, and the extension's own commands

**What it decides.** Whether a player may use a command the proxy answers: the six built-in ones
(`/server`, `/hub`, `/play`, `/send`, `/glist`, `/alert`) and the extension's own.

**Where.** `answer` in `crates/meridian-proxy/src/commands.rs:92-172` checks each command through
`Typist::has` (`:30-32`), which asks `Permissions::allows`
(`crates/meridian-config/src/permissions.rs:202`). The decision point wraps `Typist::has`. The
call is on the thread that copies the player's packets to the server: `answer_command` runs in
that loop (`crates/meridian-handoff/src/session.rs:840`, `:897-926`). While it waits, the
player's packets to the server wait too, keep-alive answers included. Hence the short default
timeout of 250 ms, and the cache below.

**Request** `meridian:decide/command`:

```json
{
  "player": {"name": "Alex", "uuid": "...", "auth": "verified"},
  "server": "lobby",
  "command": "server",
  "args": ["east"],
  "nodes": ["meridian.server", "meridian.server.east"]
}
```

`nodes` lists every node the command would check. `/server` with no name checks one node for
each server it may list (`commands.rs:210-217`), so one request asks them all.

**Answer:**

```json
{"nodes": {"meridian.server": "allow", "meridian.server.east": "deny"}, "cache_seconds": 60}
```

Each node is `allow`, `deny` or `default`, and a node left out is `default`. `default` is decided
by `permissions.toml` as today, so the six commands open to everyone stay open
(`docs/OPERATING.md` section 5.9). A denied command is answered with the text the proxy uses
today (`commands.rs:97-101`) and is not passed to the server.

**The cache.** `cache_seconds` (0 to `cache-seconds-max`) keeps each node's answer for that
player. The cache is cleared when the player leaves, at `reload`, and by the order
`meridian:permissions/invalidate` (section 5). With the cache, a player who types `/hub` ten
times costs one request.

**Precedence.** An `allow` or a `deny` of the extension wins over `permissions.toml`, and only
`default` falls back to it. This differs from admission on purpose. An operator who grants
`decide.command` is making the extension the source of rights for the proxy's commands (their
own rank system, for instance), so the extension must be able to give a node the file does not.
The file stays the answer when the extension says nothing, and the whole answer when the
extension is down and the policy is `default`. The operator decided it so (decision U2, section
10.3).

**Default without an extension:** `permissions.toml`, unchanged. **Timeout:** 250 ms.

**The extension's own commands.** With the `commands` capability, the extension declares
commands in its `hello`:

```json
"commands": [{"name": "party", "aliases": ["p"], "usage": "/party <invite|leave> [player]"}]
```

- A name that is one of the built-in commands enabled in `[commands]`
  (`crates/meridian-config/src/commands.rs:9-49`), or one another extension already declared, is
  refused and logged. The first in the configuration's order keeps it.
- A declared command is answered by the proxy and **never reaches the server**, as a built-in
  one is. Its node is `ext.<extension>.<command>` and goes through the same permission chain. With
  no rule it is open to everyone, since the extension can refuse when it runs it.
- Running it: `meridian:command/run` with the player, the server, the command and its arguments.
  The answer is `{"lines": ["...", "..."]}`: at most 10 lines of plain text, each at most 256
  characters.
- So that the relay's thread does not wait for the run, the proxy takes the command, answers
  nothing at once, and sends the lines when they come through `SessionHandle::tell`
  (`crates/meridian-handoff/src/session.rs:604`), which `/alert` already uses. This needs one
  change in `answer_command` (`session.rs:897-926`), which today always sends the hook's reply at
  once: a hook result that means "taken, the answer comes later". The timeout is 2000 ms. After
  it, the player reads `That command did not answer.`.
- The commands are offered to the client's completion only if the proxy changes the command tree
  the server sends. It does not today (`README.md:284`: `/server` is not offered by Tab
  completion). Version 1 does not add it.

### 4.4 The choice of server

**What it decides.** Which server a player goes to when the proxy places them: at login, and when
their server is lost (the fallback). Not when a player names a server (`/server east`), and not
for `/hub` and `/play` in version 1.

**Where.** At login, in `place`, between `route::candidates` and the health order
(`crates/meridian-proxy/src/session.rs:570-572`), before the balancer (`:588-598`) and the loop
that tries each candidate (`:599-627`). In the fallback, at the same step of `replace`
(`crates/meridian-proxy/src/fallback.rs:101-116`).

**Request** `meridian:decide/server`:

```json
{
  "player": {"name": "Alex", "uuid": "...", "auth": "verified"},
  "reason": "login",
  "host": "play.example.org",
  "lost": null,
  "candidates": [
    {"name": "lobby", "role": "lobby", "players": 12, "soft_cap": 20, "hard_cap": 30, "state": "open"},
    {"name": "lobby2", "role": "lobby", "players": 3, "soft_cap": 20, "hard_cap": 30, "state": "draining"}
  ]
}
```

- `reason`: `login` or `fallback`. With `fallback`, `lost` names the server the player lost, and
  `candidates` already leaves out the servers that lost them.
- `candidates`: what the routes allow, in the order the proxy would try them (`route.rs:59`).
  It is already filtered so that a verified and an unverified player never meet on one server
  (`route.rs:116`). `role`, the caps and `state` are given when balancing is on, and are null
  otherwise.

**Answer:**

```json
{"choice": "default"}
{"choice": "order", "servers": ["lobby2", "lobby"]}
{"choice": "refuse", "reason": "No game is running."}
```

- `default`: the proxy goes on as today.
- `order`: the proxy tries these servers in this order, and only these. Each must be one of the
  candidates, and an answer that names another is invalid. With balancing on, the balancer is not
  asked for this login. Each server is reserved by the rule a named server follows
  (`crates/meridian-proxy/src/placing.rs:166`, `named`), so the hard cap still holds. A server at
  its hard cap is passed over for the next one in the order. The operator decided it so
  (decision U4, section 10.3).
- `refuse`: the login is refused with the reason (at the fallback, the player is disconnected
  with it, as when no server can take them).

**Default without an extension:** the routes, the health order and the balancer, unchanged.
**Timeout:** 1000 ms at login, 500 ms at the fallback (a player who lost their server is waiting
in the configuration phase).

**With the queue.** The choice comes after the player has a place under `max-players`. If every
server the extension ordered is at its hard cap and the queue is on, the player waits as they
do today when every lobby is full (`session.rs:628-647`, `queue.rs:175`). When they are woken,
the extension is asked again.

## 5. Orders: the extension asks the proxy

The extension sends JSON-RPC requests on the same connection, once the hello is done. They mirror
the console (`crates/meridian-proxy/src/console.rs:43`, the built-in commands). Each needs its
capability, and a request without it is answered `-32601`, as if the method did not exist.

| Method | Console equivalent | Capability |
| --- | --- | --- |
| `meridian:players/list` | `list` (`console.rs:242`) | `orders.read` |
| `meridian:players/find` (name or id) | `list` | `orders.read` |
| `meridian:servers/list` (with counts, roles, caps, state) | `servers` (`console.rs:277`) | `orders.read` |
| `meridian:queue/list` | `queue` (`queue.rs:426`) | `orders.read` |
| `meridian:players/tell` (one player, a line) | none; `SessionHandle::tell` (`meridian-handoff/src/session.rs:604`) | `orders.message` |
| `meridian:broadcast` (all, or one server) | `alert` (`console.rs:456`, `staff.rs:128`) | `orders.message` |
| `meridian:players/send` (a player, all, or a server's, to a server) | `send` (`console.rs:406`, `staff.rs:18`) | `orders.move` |
| `meridian:players/kick` (a player, a reason) | `kick` (`console.rs:474`, `session.rs:395`) | `orders.moderate` |
| `meridian:servers/drain`, `meridian:servers/undrain` | `drain`, `undrain` (`console.rs:334`, `:376`) | `orders.operate` |
| `meridian:reload` | `reload` (`console.rs:501`) | `orders.operate` |
| `meridian:permissions/invalidate` (a player, or all) | none | `decide.command` |
| `meridian:console/execute` (a line) | any (`console.rs:165`, `Commands::execute`) | `orders.console` |

- **The answers are the console's own facts as JSON**, not its text. `players/send` answers when
  the moves have started, with the number of players. Each move's end comes as an event in
  version 1.1. Until then, `players/find` tells where a player is.
- **`console/execute` is the remote console.** It returns the lines the console would print. It
  is logged as `extension "<name>" ran: <line>`, as an operator's command would be, except for
  the lines that carry a secret, which the console does not have today and must not gain. With
  this capability an extension can ban, change permissions and stop the proxy: grant it only to a
  program as trusted as the operator's shell.
- **Rates.** Each extension has a budget of orders: 20 a second, with bursts of 50 (settings).
  An order over it is answered with a JSON-RPC error and is not run. `players/send` and
  `broadcast` count for each player they reach.

## 6. Events: the proxy tells the extension (version 1.1, optional)

With the `events` capability, the proxy sends JSON-RPC notifications (no id, no answer), named as
Horizon names its own (`minecraft:notification/players/joined` in Horizon `management.rs:119`):

| Notification | When | Where it starts in the code |
| --- | --- | --- |
| `meridian:notification/players/joined` | A session opens on a server | `session.rs:720` |
| `meridian:notification/players/left` | A session ends | `session.rs:741-759` |
| `meridian:notification/players/moved` | A switch ends (seamless, cold, or failed, and why) | `switching.rs`, `staff.rs:18` |
| `meridian:notification/servers/up`, `.../servers/down` | The sidecar's status of a server comes back or fails | `crates/meridian-proxy/src/status.rs:145`, `:161` |
| `meridian:notification/players/fallback` | A player is moved off a lost server | `fallback.rs:58` |
| `meridian:notification/queue/entered`, `.../queue/left` (with the cause) | A player enters or leaves the line | `queue.rs:212`, `:270` |

- They go through a bounded queue for each extension, with one writer thread, so that no player's
  thread waits on a slow extension. Horizon's management protocol does the same for its
  notifications: a queue and a thread, so that no caller waits on a slow client (Horizon
  `crates/horizon-server/src/management.rs:27-31`, `Clients`).
- When the queue is full, the newest are dropped and counted, and the next event to go out is
  `meridian:notification/events/dropped` with the count. An extension that needs to be exact
  asks `players/list` again.
- Events are never a decision. An extension that wants to refuse something uses a decision
  point.

## 7. Security, data and logs

**Who is trusted.** An extension holds the token the operator gave it and the capabilities the
operator granted. It is as trusted as those capabilities make it, and no more. A program on the
same machine without the token can open the port but learns nothing (section 2.2).

**What an extension can see:**

- names, unique ids and how they were checked (`auth`);
- the server each player is on, the servers, their roles, caps, counts and states;
- the host name a player typed;
- the commands of the proxy players type, with their arguments (only with `decide.command` or
  `commands`, and only the proxy's own commands, never what goes to a server);
- players' addresses, **only** with the `addresses` capability (decision U7, section 10.3).

**What it never sees:** the forwarding secret (`crates/meridian-config/src/lib.rs:162`), any
extension's token, the sidecars' secrets, a player's session keys or encryption, their profile's
signed properties, their chat, or any packet. No method, capability or event carries them, and a
test checks that the forwarding secret appears in nothing the proxy sends to an extension.

**Limits:**

- 256 KiB for each message;
- `max-in-flight` for decisions;
- the order budget (section 5);
- the bounded event queue;
- 10 lines of 256 characters for a command's answer.

A message over a limit closes the connection, logged once.

**Logs and metrics.**

- One line at each connection, with the protocol version, the extension's name and version
  from its hello, and the capabilities granted and refused.
- One line when the breaker opens or closes, and one at each refused or invalid answer, with the
  method and the reason, never the whole message.
- At `debug`, each decision with its outcome and time.
- Metrics, beside the existing ones (`docs/OPERATING.md` section 8.1):
  - `meridian_extension_connected{extension}`;
  - `meridian_extension_requests_total{extension,method,outcome}`, where the outcome is
    `answered`, `timeout`, `invalid`, `busy`, `breaker` or `disconnected`;
  - `meridian_extension_request_seconds{extension,method}`;
  - `meridian_extension_breaker_open{extension}`;
  - `meridian_extension_orders_total{extension,method,outcome}`;
  - `meridian_extension_events_dropped_total{extension}`.

## 8. The Rust SDK: `meridian-extension`

A crate of the workspace, published with Meridian's releases. It is a library for writing an
extension, and the examples are binaries in it. It uses no unsafe code and blocks on threads, as
the proxy does, with no async runtime. The surface below is a sketch in signatures, to be settled
in its pull request.

### 8.1 Surface

```text
pub trait Extension: Send + Sync + 'static {
    fn login(&self, request: &LoginRequest) -> LoginAnswer            { LoginAnswer::Default }
    fn command_permission(&self, request: &CommandRequest) -> NodeAnswers { NodeAnswers::default() }
    fn choose_server(&self, request: &ServerRequest) -> ServerAnswer  { ServerAnswer::Default }
    fn run_command(&self, request: &RunRequest, proxy: &Proxy) -> Vec<String> { Vec::new() }
    fn event(&self, event: &Event, proxy: &Proxy)                     {}
}

pub struct Builder;   // Builder::new(name, version)
                      //   .listen(addr)            loopback only, as the proxy checks
                      //   .token_file(path)        or .token(Secret)
                      //   .wants(&[Capability])    declared in the hello
                      //   .command(CommandSpec)    its own commands
                      //   .threads(n)              how many requests it answers at once
                      //   .run(extension) -> Result<(), Error>

pub struct Proxy;     // orders, from any thread:
                      //   players() -> Result<Vec<Player>>, find(&str) -> Result<Option<Player>>,
                      //   servers() -> Result<Vec<Server>>, tell(&Player, &str), broadcast(Option<&str>, &str),
                      //   send(Target, &str), kick(&Player, &str), invalidate(Option<&Player>),
                      //   console(&str) -> Result<Vec<String>>

pub enum LoginAnswer  { Allow, Deny(String), Default }
pub enum NodeAnswer   { Allow, Deny, Default }
pub struct NodeAnswers { pub nodes: Vec<(String, NodeAnswer)>, pub cache_seconds: u32 }
pub enum ServerAnswer { Default, Order(Vec<String>), Refuse(String) }
pub enum Auth         { Verified, Unverified }
pub struct Player     { pub name: String, pub uuid: Uuid, pub auth: Auth, pub server: Option<String>,
                        pub address: Option<IpAddr> }
```

- Each method runs on one of the SDK's threads. The SDK keeps the deadline. An answer that would
  arrive after the proxy's timeout is not sent, and the SDK logs that it was late.
- A panic in a method is caught per request (`catch_unwind`), answered as an internal error, and
  the extension goes on. The proxy then applies its on-error policy.
- The handshake, the proofs, the limits and the validation are the SDK's. An extension's code sees
  only typed requests.
- The wire types live in one module that the proxy uses too, so that the two cannot drift. The
  proxy depends on that module, not on the SDK's server.

### 8.2 Two examples

- **`allow-all`.** It answers `Default` to everything and logs each request with its time. It is
  the smallest extension that runs, the template to copy, and the one the proxy's integration
  tests start. It shows that the proxy with an extension that never disagrees behaves as without
  one.
- **`file-permissions`.** It reads a TOML file of players (by unique id) and the nodes they are
  given or denied. It answers `decide.command` from it, with `cache_seconds` 60. It declares one
  command, `/extperms reload`, under its own node, which reads the file again and sends
  `permissions/invalidate`. It shows the cache, the precedence over `permissions.toml`, a command
  of the extension's own, and an order.

### 8.3 Tests

- **The wire:** golden JSON for each request and answer of protocol 0, in the repository, read
  by the SDK's tests and the proxy's, so that both sides check the same bytes.
- **The handshake:** test vectors for the two proofs, a wrong token on each side, and a server
  that is not the extension (the proxy sends nothing after a wrong proof).
- **The SDK:** a fake proxy drives an extension through every method. It checks a late answer,
  a panic, the limits, and an unknown field that must be ignored.
- **The proxy, with a fake extension** (as the tests already fake backends:
  `crates/meridian-proxy/src/session.rs:947`):
  - each decision point's answers;
  - each on-error policy, for a timeout, an invalid answer, a closed connection and `busy`;
  - the breaker opening and closing;
  - the order of the login checks (a banned player is never asked about; a refused second login
    does not end the first session; a refused player never enters the queue);
  - the time budget check of the configuration;
  - an order without its capability;
  - the rate budget;
  - no forwarding secret in anything sent.
- **Unchanged without extensions:** the existing tests run as they are, with no
  `[[extension]]`.
- **Both systems:** CI already runs on Linux and Windows (`.github/workflows/ci.yml:19`), and
  the loopback tests run on both.

## 9. The built-in modules

The bans and whitelist (#81), the permissions (#82), the queue (#83) and the fallback (#84) stay
as they are: off until configured (except the fallback, on by default as today), unchanged in
size, and each the answer of its decision when no extension is configured, when the extension
says `default`, and when the on-error policy is `default`.

**Precedence, in one place:**

| Decision | Built-in | Extension |
| --- | --- | --- |
| Admission | A ban or the whitelist refuses first, and it is final | Can refuse a player the built-ins let in; cannot let in a player they refuse |
| Permission of a proxy command | `permissions.toml` decides each node the extension leaves at `default` | `allow` or `deny` of a node wins |
| Choice of server | Routes, health and the balancer, when the extension says `default` | `order` replaces them for that placement, inside the routes and the hard caps; `refuse` refuses |
| Queue and player limit | Always the proxy's | Not asked in version 1 (a priority place may come later, below) |

**A priority place in the queue, later.** Version 1 gives an extension no say in the queue
(decision U5, section 10.3). It can be added later without breaking anything: an optional field
of the admission answer, such as a priority, added in a minor version of the protocol once the
queue has a second level (players who pass ahead of the others) and an operator needs it.
Section 3 allows it: a field added inside a version is optional, and both sides ignore the fields
they do not know. An extension that never sends it and a proxy that does not read it both go on
working.

Later, the built-in modules could be written again as extensions shipped with Meridian, so that
the proxy itself has one way to decide. That is not proposed now. They work, they are tested,
and moving them would put a socket in the path of every login of every operator, including
those who need none.

## 10. Plan, risks, decisions and questions

### 10.1 Pull requests

Each one is small, keeps the proxy as it is without `[[extension]]`, and passes CI on Linux and
Windows.

1. **This page**, and a line in the README. It proves nothing in code. It is the reference the
   others follow.
2. **The configuration.** `[[extension]]` in `meridian-config`: the reading, the loopback check,
   the grants, one extension for each decision, the defaults, and the login time budget added to
   the queue's check. Nothing uses it. Proof: the configuration tests, and the existing tests
   unchanged. CONFIG.md says what that budget means for the operator: the `timeout-ms` of
   `decide-login` and `decide-server` count with `wait-seconds` and `connect-timeout-ms` against
   the 27 s a login may stay silent, so with an extension on both points the queue's longest wait
   is shorter than without.
3. **The wire.** The types of protocol 0, the golden JSON, the proofs of the handshake (with the
   HMAC moved to where both sides reach it), and the WebSocket code both sides need: the
   sidecar's, extended and shared as `meridian-websocket` (section 2.5, decision U6). It also
   corrects the head comment of that code, which called it "a client which only listens". Proof: the golden files, the test vectors, and the sidecar's tests unchanged.
4. **The connection on the proxy.** Connect, hello, reconnect with the doubling wait,
   `max-in-flight`, the breaker, the metrics and the logs. No decision is wired yet. Proof: tests
   against a fake extension, including a wrong token on each side.
5. **Admission.** The point in `serve`, in the order of section 4.2. Proof: the tests of each
   answer and policy, and of the order with the bans, the second login and the queue.
6. **The permission of a command, and the cache.** It wraps `Typist::has`. Proof: tests with the
   six commands, the cache and its invalidation, and the precedence over `permissions.toml`.
7. **The extension's own commands.** The hello's declaration, the change to `answer_command` in
   `meridian-handoff`, and `command/run`. Proof: a declared command never reaches the fake
   backend, a clash is refused, and the late answer reaches the player through `tell`.
8. **The choice of server.** At login and in the fallback, with balancing on and off, and with
   the queue. Proof: the tests of `order`, `refuse` and an invalid name, and the hard cap held.
9. **Orders.** The methods of section 5, their capabilities and the rate budget. Proof: one test
   for each method against the code the console uses, and the refusals. This pull request settles
   what happens when an answer would pass the limit of 256 KiB a message (section 2.1), which a
   `players/list` of a large network reaches near 1,700 players: pages, or a higher limit.
10. **The SDK and `allow-all`.** Proof: the SDK's tests, and the proxy's integration test with
    `allow-all` started as a process on both systems.
11. **`file-permissions`.** Proof: its tests, and an integration test of a rank given by the file.
12. **The operator's documentation.** OPERATING.md and CONFIG.md gain a section, and the README's
    "Extensions" row changes to the state reached. DESIGN.md sections 13.5 and 14 and PARITY.md's
    item 9 are aligned with this page.
13. **Events** (version 1.1). The notifications of section 6, the bounded queue and the drop
    count. Proof: tests of each event and of the drop under a slow extension.

### 10.2 Risks

- **A decision on the relay's thread.** A slow permission answer delays the player's packets to
  their server. Mitigation: 250 ms by default, the cache, the breaker, `busy`.
- **The login's silence.** The client leaves after 30 s, and a slow extension adds to the queue's
  wait. Mitigation: the budget check of the configuration (section 4.2).
- **A port taken by another program.** Mitigation: the proofs both ways, so nothing is sent to
  it.
- **The extension as an attack path.** `orders.console` is as strong as the operator's shell.
  Mitigation: capabilities granted one by one, and `console/execute` logged.
- **Scope creep.** Each new request is weighed against the rule of section 1.2. A request that
  makes the proxy own a server's matter is refused, and the extension is the answer.
- **A WebSocket server in the SDK.** It is more code than the sidecar's client. If it costs too
  much, JSON lines over TCP (section 2.4) keep the same messages and change only the framing.
- **Two systems.** Loopback names resolve differently (`localhost` may give `::1` first on
  Windows). Mitigation: the configuration takes an address, the check of every resolved address
  is the sidecar's, and CI runs on both.

### 10.3 Decisions of the operator (2026-11-05), with the reason this specification gives for each

- **U1. The proxy connects to the extension, which listens on the loopback** (section 2.1). The
  proxy opens no new port, and no program becomes an extension unless the configuration names
  it.
- **U2. An extension's `allow` or `deny` wins over `permissions.toml`** for the proxy's commands
  (section 4.3). Granting `decide.command` makes the extension the source of rights for those
  commands, and the file answers only what it leaves at `default`.
- **U3. One extension for each decision point in version 1** (section 2.3). One answer for each
  decision keeps its order, its deadline and its failure simple to follow.
- **U4. An extension's `order` replaces the balancer for that placement, inside the hard caps**
  (section 4.4). An operator's matchmaker must be able to put a player where it wants, and the
  hard cap still protects each server.
- **U5. No priority place in the queue in version 1** (section 9). It can come later, as an
  optional field of the admission answer in a minor version, once the queue has a second level
  and an operator needs it; section 3's rule on added fields keeps that compatible.
- **U6. The sidecar's WebSocket code is extended and shared as `meridian-websocket`, with no new
  dependency** (section 2.5). One implementation serves the sidecar, the proxy and the SDK, and is
  tested on both systems.
- **U7. Players' addresses go only to extensions granted `addresses`** (section 7). An address
  is personal data that most extensions do not need.
- **U8. The names stay:** the crate `meridian-extension`, the configuration `[[extension]]` and
  the methods `meridian:...`.

### 10.4 Questions for Kacper (Horizon)

Asked on Horizon's issue #7484, comment 6657883380 (2026-11-05), and answered in comment
6679084547 (2026-11-06), except K3. Nothing on this
page changes Horizon's management protocol, and none of its methods is Meridian's: an extension
speaks a separate protocol, with its methods under `meridian:`, which borrows only the form of
Horizon's (JSON-RPC 2.0 over a WebSocket).

- **K1.** Does Horizon have a preference, or a plan of its own, for how an operator plugs their
  own system into a proxy? If Horizon would one day call such a program itself, sharing this
  shape (JSON-RPC 2.0 over a WebSocket, `rpc.discover`, the hello) would let one extension serve
  both.
  **Answered.** Horizon's maintainer chose the external program over a local socket, so that
  crashes stay out of the proxy, and JSON-RPC with `rpc.discover` like the management protocol.
- **K2.** Horizon's management protocol serves operators (`operators/add`, `operators/remove`,
  `operators/set`, `operators/clear` in Horizon `crates/horizon-server/src/management_schema.rs:39`
  and the lines around it), as well as the allowlist and bans. Would Horizon rather Meridian read
  the operators from the servers, for instance to give them the proxy's staff commands, than keep
  a list or ask an extension?
  **Answered.** Yes: read the operators from the servers and keep no separate list in Meridian.
  The proxy mirrors and the servers own them. This is built in the operators of the servers
  (`[operators]`, [OPERATING.md](OPERATING.md) section 5.9), not through an extension.

And one this page adds:

- **K3.** Horizon checks a command with vanilla's permission levels. Is a hook planned where a
  server asks someone else whether a player may run a command? If so, an operator's rank system
  could answer both the proxy (section 4.3) and the servers with one program.
  **Not answered** in that comment, so still open. Nothing on this page depends on it.
