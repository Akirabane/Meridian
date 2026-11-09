# Extensions

This page specifies how a program of the operator's own can take part in what the proxy
decides: whether a player may enter, whether they may use a proxy command, and which server
they go to. Part of it is built, and the pull requests of section 10 follow the rest. Built: the
admission at login (4.2), the permission of a command and the extension's own commands (4.3) and the
choice of server, at login and at the fallback (4.4), with the Rust SDK (`meridian-extension`), and the
dispatcher of the orders with the first one, `permissions/invalidate` (5), and the half of the SDK that sends
orders (8.1), with the orders that read, those that message, the one that moves, the one that expels, those that operate and the one that runs console lines (5); the "As built" paragraphs of 4.3, 4.4, 5 and 8.1 say what the code does where this page
leaves a choice open. Built of the events the proxy sends the extension (6): the bus, the queue of each connection, `events/dropped`,
`players/joined`, `players/left`, `players/moved`, `players/fallback`, `servers/up`, `servers/down`, `queue/entered` and `queue/left`: all the events of the table are built.

**Status: decided.** The operator settled its choices on 2026-11-05 (section 10.3). Horizon's
maintainer then agreed to the form on 2026-11-06: an external program on a local socket, speaking
JSON-RPC with `rpc.discover` as the management protocol does (comment 6679084547 on Horizon's
issue #7484). The questions asked of Horizon are in section 10.4, with what was answered.

Every statement about the code names a file and a line, checked by searching the code at `e2211c5`
(main after pull request #98): the line numbers are those of that commit and have moved since, so
search for the name next to a number. Where a statement names a function instead, it is the name that
holds today. Statements about Horizon name its files on its
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
- `allow`: let the player in, or let the command run (**as built, not for the permission of a command**:
  `allow` there is refused when the configuration is read, see 4.3);
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

The one parameter of the call is named `request`, as `rpc.discover` lists it: `params` is
`{"request": {...}}`, with the object above inside. The bytes are `golden/protocol-0/login.request.json`
and the three `login.answer.*.json` of `meridian-extension`, which the proxy's tests and the SDK's both
read; `meridian_extension::decide` has the types.

- `auth`: `verified` (checked with the session service) or `unverified` (offline mode, or the
  unverified players of the hybrid mode). In the second case **the name and the id prove
  nothing**: the extension is told so in the field, as the operator is in
  `docs/OPERATING.md` section 5.8.
  An extension that meets an `auth` it does not know (a later version of the proxy may add one) reads
  it as `unverified`: nothing is taken as proved that was not said to be.
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
`deny` refuses the login with the reason. With no reason (or an empty one), the player reads `You cannot join
this server right now.`. A reason is plain text of at most 256 characters with no control character but
the line break; an answer whose reason is not, or whose `decision` is none of the three words, is
invalid (section 4.1), but a `reason` on `allow` or `default` is not read, whatever it is. `Reason::new` in the SDK cuts and cleans a reason to what the proxy accepts, so
that an extension that means to refuse is never turned into a failure that the on-error policy lets in. The refusal goes through `refuse` (`session::refuse`), is logged as
the bans' refusals are (`session.rs:494`), and counts as
`meridian_login_failures_total{reason="extension"}`.

**An extension can only add a refusal.** `allow` is not a pass: the bans and the whitelist are
settled before the extension is asked, so an `allow` never lets in a player they turned away (a
banned player costs the extension no request at all), and the second login, the player limit and the
queue, and the choice of server still follow it. `default` and `allow` are the same for the checks;
they differ in the metrics and the log. The tests of `meridian-proxy` (`session/tests/extension.rs`)
check each of these: an `allow` for a banned player, for one the whitelist keeps out and for one who
is connected already.

**What is asked, and of whom.** The `[[extension]]` that is enabled and is granted `decide.login`
(at most one). Nothing is asked when none is, nor when the extension did not declare `decide.login` in
its hello (granted but not declared is not used). When there is no usable answer (no connection, an
open breaker, no free place, a timeout of `timeout-ms`, an invalid answer, or an extension a reload
added and has not started), the player takes the `on-error` policy of `[extension.decide-login]`:
`default` and `allow` let the player go on, `deny` refuses with the proxy's own words. The address of
the player is in the request only if the operator granted `addresses` and the extension declared it.
The refusal is counted as `meridian_login_failures_total{reason="extension"}`, and what became of each
decision as `meridian_extension_decisions_total{extension,point,decision}` (`allow`, `deny`, `default`,
or `policy_allow`, `policy_deny`, `policy_default` for want of an answer).

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

**As built.** The point is wired in the hook that answers the commands a player types
(`commands::hook_with`), through `extension/command.rs` in the proxy and `meridian_extension::decide`
(`CommandRequest`, `NodeAnswers`, `parse_command_answer`) for both sides. The golden files are
`golden/protocol-0/command.*.json`, and the SDK registers it with `Extension::on_command`. The own commands
of an extension are built too, as the last paragraph of this section says; this is the permission only.
What the specification above leaves open:

- **The rule is one function, `merge`:** `allow` is a permission **even if `permissions.toml` or the
  operators of the server refuse it**, `deny` is a refusal even if they give it, and only `default` (or a
  node the answer leaves out) leaves the question to them.
- **Guardrails, which the operator must know before granting `decide.command`:**
  1. **Only an extension the operator granted `decide.command` to (`grant`) decides**, and only one: an
     extension that is not granted it, is switched off (`enabled = false`) or did not declare it in its
     hello is never asked, whatever it says.
  2. **An `allow` of the extension can give `/send`, `/alert` and the other commands to any player.
     The token of such an extension is therefore an administrator's credential for the network: keep
     its file readable by the proxy and the extension only, as the token of the console would be.**
  3. **It concerns the proxy's own commands only** (`/server`, `/hub`, `/play`, `/send`, `/glist`,
     `/alert`; any other command goes to the server untouched and the extension is not asked). The
     login stays as 4.2 says: the bans and the whitelist come first and are never bypassed by an
     extension.
  4. **`cache-seconds-max` bounds how long an `allow` or a `deny` is kept**, whatever the extension asks
     for; with 0 nothing is kept and each command asks.
- **`on-error = "allow"` is refused for this point, when the configuration is read** (an addition to 4.1,
  which lets the other two points `allow`). With no usable answer (the extension down, slow or saturated) it
  would give every node of the command to whoever typed it, `/send` and `/alert` included: a fail-open on
  the system of rights. The error says so (`would give every command of the proxy to anyone while the
  extension is down: use "default" or "deny"`). The two values left are `"default"` (the file and the
  operators decide, which is the default) and `"deny"`. The login and the choice of server keep `allow`: the
  bans, the whitelist and the caps come before them, and there it only lets the proxy's own policy go on.
  Nothing that fails ever grants a node: the proxy reads any other policy as `default`.
- **The request** asks about the nodes the command checks and the proxy has no kept answer for, in one
  call (`nodes_of` lists them: `/server` with no name has one for each server the player may be sent to,
  `/hub` its own and that of the server it leads to). Only the nodes asked are read from the answer, so
  what a session keeps cannot grow with what an extension sends. At most 32 arguments are told, each cut
  to 256 characters; they are what the player typed and are not to be trusted.
- **The call waits for the answer on the thread that copies the player's packets**, at most `timeout-ms`
  (250 ms by default), and only for a command the proxy answers. A player who types commands that miss
  the deadline holds up only their own packets, and five misses in a row open the breaker, after which
  the commands take their policy at once with no request.
- **What is kept belongs to the player's session** (it goes when they leave, and nothing in the proxy
  holds it afterwards). A reload and a new hello of the extension drop it, and so does a change of the
  player's server. A failure is never kept.
- **What a kept answer is kept for.** The extension decides with the whole request, so an answer is kept
  for that request and no other: the key is the server the player is on, the command and its arguments
  (as the extension was told them) and the node. `/send Bob lobby` and `/send all east` are two
  questions; a verdict given on one server is not used on another. So an extension may let the proxy
  keep a verdict that depends on the server, the command or its arguments; what it must not do is give
  `cache_seconds` to a verdict that depends on something the request does not carry (the time of day,
  a rank it changes meanwhile): that verdict is kept as long as it said, and only a reload, a new hello
  or the end of the session ends it earlier. The proxy keeps at most 512 answers for a session, and drops
  those that ran out before it refuses a new one.
- The hook reads the commands' own settings (`[commands]`, the nodes a command checks) as they were when
  the player logged in, as it did before extensions, and the gate reads the extension's settings
  (`decide-command`, which extension is granted the point) as they are now.
- The decisions are counted as `meridian_extension_decisions_total{point="command"}` with `allow`, `deny`,
  `default` (what the extension answered for the command: `deny` if it denied any node, else `allow` if
  it allowed any), `cached` (every node was kept) and, with no usable answer, `policy_default` or
  `policy_deny`.

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
  (in `crates/meridian-handoff/src/session.rs`), which `/alert` already uses. This needs one
  change in `answer_command` (`crates/meridian-handoff/src/session.rs`), which today always sends the hook's reply at
  once: a hook result that means "taken, the answer comes later". The timeout is 2000 ms. After
  it, the player reads `That command did not answer.`.
- The commands are offered to the client's completion only if the proxy changes the command tree
  the server sends. It does not today (`README.md:284`: `/server` is not offered by Tab
  completion). Version 1 does not add it.

**As built, for the extension's own commands:** the wire, the SDK, the relay's side and the proxy's.
`meridian_extension::run` has the request (`RunRequest`: the player, the server, the command's **declared
name** (an alias resolved), and the arguments, at most 32 of 256 characters as in `decide/command`), the
answer (`Lines`, `parse_run_answer`) and the one check of a line (`check_line`: at most 256 characters, no
control character) that the SDK makes its lines with, so that an extension cannot send one the proxy would
refuse. `wire::CommandSpec::check` is the one check of a declaration: a name and aliases of 1 to 32 characters
among `a-z`, `0-9`, `-` and `_`, none given twice. The golden files are `golden/protocol-0/run.*.json`.
The SDK declares a command with `Extension::command` (one method, `command/run`, serves all of an extension's
commands; a word declared twice or not valid panics, as a method registered twice does), and
`examples/command_party.rs` is a small one. The relay's hook can take a command and answer it later
(`meridian_net::relay::TAKEN`).

In the proxy, `extension/own.rs` takes the commands, in the hook of the player's commands (`commands::hook_with`),
before the proxy's own are looked at:

- **Which words are an extension's.** The declarations of the extensions that are enabled and granted
  `commands` are read in the **order of the configuration**, not of the hellos (`Registry::claim`), and again
  whenever one of them, or the commands of the proxy that are enabled, change (see the bounds of a hello). A word that is
  not valid, that is a command of the proxy enabled in `[commands]` (`ProxyCommand::words`: `/lobby` counts), or
  that an extension before has, is refused and logged once for each hello. A name that is refused refuses its
  command; an alias that is refused is dropped. A word is matched as typed, as the proxy's own are. A command
  stays the extension's **also while the extension is down** (its declaration of the last hello is kept): the
  player reads `That command did not answer.`, and the server never sees the word. **This is decided:** the
  word stays reserved as long as the extension is configured, enabled and granted `commands`; before the first
  hello after the proxy starts the word is unknown, so it goes to the server.
- **The permission** is the node `ext.<extension>.<command>` (lower case, the same for every alias), **open to
  everyone** unless a group denies it. `Typist::has` and `merge` decide as for the proxy's own commands, with the
  verdict of the extension that holds `decide.command` **only if it is the extension that owns the command**
  (`Gate::verdicts_of`): what a player typed after a command is not told to another extension. A player who may
  not use it reads `You do not have permission to use /<word>.`, and the extension is not asked.
- **The run.** The hook returns `TAKEN` at once and a thread of its own makes the call, `meridian:command/run` with a
  deadline of `[extension.commands] timeout-ms` (2000 ms), then tells each line through the session's handle. The
  arguments are bounded as `decide/command`'s (32 of 256 characters). A player has **one command running at a
  time**: a second reads `Still working on your last command.` An answer that does not pass `parse_run_answer`
  (more than 10 lines, a line over 256 characters or with a control character), a deadline that passes, a call that
  finds no connection, a full `max-in-flight` or an open breaker all end the same way: `That command did not answer.`
- **The breaker.** A command counts **neither for nor against** the extension's breaker (`Link::call_counting`): a
  failure does not count, so that one slow command does not turn every login and permission to its on-error
  policy, and an answer does not reset the count of failures in a row, so that an extension whose logins time out
  and whose commands answer still opens its breaker. A command is only refused while the breaker is open (no
  request goes), and is never the trial of one: the next decision is.
- **The places.** The commands of an extension share its `max-in-flight` places with its decisions: at most half
  of them run commands (`Link::run_place`), so that players who type a slow command cannot leave a login or a
  permission without a place. One that finds none is told `That command did not answer.` at once, and counted as
  `busy`.
- **What the player reads.** A line holds no formatting code (`§`), no control character, and no mark that
  reverses or hides text (direction marks, characters of no width): `Lines::new` drops them and the proxy
  refuses an answer that has one (`run::is_refused`). **This is a prudent default**: colours would need a field
  of its own, allowed by the operator, added later.
- **The characters of a line.** `run::is_refused` lists them: control characters, `§`, direction marks and
  separators, the soft hyphen, the combining grapheme joiner, the Hangul fillers, the characters of no width, the
  variation selectors (an emoji that one turns into a picture shows as its plain form), the interlinear annotation
  marks and the tag characters. **A known limit of this prudent default**: taking out the zero-width non-joiner and
  joiner (U+200C, U+200D) breaks the joins and the separations of letters in Persian and in some Indic scripts, and
  taking out the left-to-right and right-to-left marks (U+200E, U+200F) can change the order in which Arabic or
  Hebrew mixed with Latin text reads. An extension that has to write such text needs the field that would relax the
  rule, which is not built.
- **The places for commands.** An extension granted `commands` needs `max-in-flight` of at least 2 (its commands use at
  most half of its places, so that its decisions keep one); a configuration with 1 is refused when it is read.
- **The bounds of a hello.** At most 64 commands, 8 aliases for each and a `usage` of at most 256 characters; the
  words are resolved again when an extension says hello, when a reload replaces its connection (a change of its
  address, token file, grant, `max-in-flight` or breaker settings gives it a new link), when the order of the
  extensions or which of them are granted `commands` changes, and when the commands of the proxy that are
  enabled change, and not for every command a player types: the proxy compares what the words were resolved
  for (the extensions, each with its link and how many hellos it has been through) and looks the word up.
- **The deadline** `[extension.commands] timeout-ms` is 1 to 10000 ms.
- **Not built:** the commands are not offered to the client's completion, and the metrics have no label by command
  (`meridian_extension_requests_total{method="meridian:command/run"}` counts the runs and their outcomes).
- The names of two extensions that differ only in case are refused when the configuration is read (the node is lower case).

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

**As built.** The point is wired at login (`place` in `session.rs`, after the route and the health
order, before the balancer) and at the fallback (`replace` in `fallback.rs`), through
`extension/server.rs` in the proxy and `meridian_extension::decide` (`ServerRequest`, `Choice`,
`parse_server_answer`) for both sides. The golden files are `golden/protocol-0/server.*.json`, and
the SDK registers it with `Extension::on_server`. What the specification above leaves open:

- `candidates` carries, for a server the balancer knows (it has a `role`), its `role`
  (`lobby`, `sublobby` or `game`), its `players` (the sessions on it and the places held for
  players being sent there), its caps as the balancer uses them (a cap the server reports lower is
  the one given), and its `state`: `open`, `closed` (at its soft cap), `draining` (drained from the
  console, restarting, or not accepting transfers) or `not_offered` (no recent report from it). For a
  server with no role, or with balancing off, everything but `name` is null. A state word a later
  version adds is to be read as `closed`.
- An `order` must name at least one server, each one of the `candidates` and spelled as it is; a name
  twice counts once, at its first place. An empty list, a name that is not a candidate, or a name that
  is not text is an invalid answer: it counts toward the breaker and the on-error policy applies
  (`default`, or `refuse`). The extension cannot send the proxy to a server it was not offered.
- The servers of an order are tried in that order, among them those with no role as well (nothing is
  held for them). A server that is full, drained, not accepting players or not heard from is passed
  over for the next, and one that does not answer is passed over as in the ordinary loop. When none
  could take the player for want of room, the player is told the server is full, or waits if the queue
  is on; when none answered, they are told which could not be reached, and only those the order named
  are listed.
- At the fallback the wait for the extension is the smaller of `timeout-ms` and 500 ms. **There a
  failure is never a refusal**: the player is already in the game, and a server that is lost while the
  extension is down is the case where players must not be thrown out. A timeout, an invalid answer, an
  extension that is not connected or an open breaker take the policy `default` whatever `on-error`
  says, and the player is placed as without an extension. Only an explicit `refuse` of the extension,
  which is its decision, disconnects the player, with its reason.
- An `order` may send a player to a server at its soft cap (state `closed`), up to its hard cap: the
  extension decides where the players go, and the balancer's soft cap and margin are the balancer's own
  rule, not its. The hard cap always holds.
- The decisions are counted as `meridian_extension_decisions_total{point="server"}` with
  `default`, `order`, `refuse` and, when there was no usable answer, `policy_default` or
  `policy_refuse`. A refusal at login counts as `meridian_login_failures_total{reason="extension"}`.
- While the extension is asked, the player holds a place under `max-players`, so a slow extension at
  1000 ms costs that much of the place of each player who waits on it; `max-in-flight` bounds how many.

## 5. Orders: the extension asks the proxy

The extension sends JSON-RPC requests on the same connection, once the hello is done. They mirror
the console (`crates/meridian-proxy/src/console.rs:43`, the built-in commands). Each needs its
capability, and a request without it is answered `-32601`, as if the method did not exist.

| Method | Console equivalent | Capability |
| --- | --- | --- |
| `meridian:players/list` | `list` (`console.rs:242`) | `orders.read` |
| `meridian:players/find` (name or id) | `list` | `orders.read` |
| `meridian:servers/list` (with counts, roles, caps, state) | `servers` (`console.rs:277`) | `orders.read` |
| `meridian:queue/list` | `queue` (`queue::command`) | `orders.read` |
| `meridian:players/tell` (one player, a line) | none; `SessionHandle::tell` (`meridian-handoff/src/session.rs`) | `orders.message` |
| `meridian:broadcast` (all, or one server) | `alert` (`console.rs:456`, `staff.rs:128`) | `orders.message` |
| `meridian:players/send` (a player, all, or a server's, to a server) | `send` (`console.rs:406`, `staff.rs:18`) | `orders.move` |
| `meridian:players/kick` (a player, a reason) | `kick` (`console.rs:474`, `Sessions::kick` in `session.rs`) | `orders.moderate` |
| `meridian:servers/drain`, `meridian:servers/undrain` | `drain`, `undrain` (`console.rs:334`, `:376`) | `orders.operate` |
| `meridian:reload` | `reload` (`console.rs:501`) | `orders.operate` |
| `meridian:permissions/invalidate` (a player, or all) | none | `decide.command` |
| `meridian:console/execute` (a line) | any (`console.rs:165`, `Commands::execute`) | `orders.console` |

- **The answers are the console's own facts as JSON**, not its text. `players/send` answers when
  the moves have started, with the number of players. Each move's end comes as a
  `players/moved` event (section 6); `players/find` tells where a player is.
- **`console/execute` is the remote console.** It returns the lines the console would print. It
  is logged **before** it runs (`extension "<name>" runs: <line>`) and what it said after (`ran: <line>: ...`), as an
  operator's command would be, except for the lines that carry a secret, which the console does not have today and must
  not gain. With this capability an extension can ban, change permissions and stop the proxy: grant it only to a
  program as trusted as the operator's shell. How it is built, and what it cannot do, is in the "As built" paragraph of
  section 5, for the order that runs console lines.
- **Rates.** Each extension has a budget of orders: 20 a second, with bursts of 50 (settings).
  An order over it is answered with a JSON-RPC error and is not run. `players/send` and
  `broadcast` count for each player they reach, **at most the burst**: a `send all` to 500 players
  costs the whole burst, so that it can run (it would cost more than the budget ever holds
  otherwise).

**As built, for the dispatcher and `meridian:permissions/invalidate`** (`extension/orders.rs`,
`meridian-extension/src/orders.rs`):

- **Which method, which capability.** A table gives each order its capability. A method that is not in
  it, and a method whose capability the connection was not granted (asked for in the hello **and**
  granted in the configuration), are answered the same way, `-32601 method not found`: an extension
  cannot tell a method it may not use from one that does not exist. A request with no `id` is a
  notification: nothing is run for it and nothing is answered.
- **An order never runs on the thread that reads the connection.** That thread also hands the proxy's
  own requests their answers, and a slow order must not hold them. The reader takes a place among the
  extension's `max-running` and one token of its budget, and starts the order on a thread of its own,
  which writes the answer and gives its place back **before** it writes, so that an extension that
  sends its next order when it reads the answer finds the place free. A write that blocks (an extension
  that does not read its answers) holds its thread, not a place, for the write timeout: the threads
  that are alive, running or waiting to write, are bounded to twice `max-running`, and an order that
  would make one more is answered `-32002` like one that finds no place. A handler that panics is
  answered `-32603 the order failed`, and the connection goes on.
- **Errors.** `-32602` for parameters that are not what the order takes (the message says what is
  wrong in a few words and never quotes what was sent); `-32001 over the budget of orders` when the
  budget is spent; `-32002 too many orders running` when `max-running` are running. The last two are in
  the range JSON-RPC keeps for the errors of a server. Neither ran, and asking again later is fine.
  **A refused `permissions/invalidate` did not forget anything**: an extension that has just lowered
  someone's rights must ask again until it is answered `{"ok": true}`, because the permission it
  meant to take back is kept for at most `cache-seconds-max` otherwise. The
  budget is a token bucket per extension, kept while the proxy runs (a new connection does not refill
  it); `rate-per-second` tokens come back each second, up to `burst`.
- **`rpc.discover`** of the proxy lists the orders the connection was granted the capability of, and
  nothing for the ones it was not. The SDK sends them with a `Proxy` handle (8.1).
- **`meridian:permissions/invalidate`** (capability `decide.command`) takes exactly one of
  `{"player": "<unique id>"}` (with or without hyphens, in either case) or `{"all": true}`, and answers
  `{"ok": true}`. It makes the proxy forget the permissions it kept of the extension's answers
  (`cache_seconds`, 4.3): for one player, or for everyone. It is for an extension that has just
  changed someone's rights and does not want to wait for the answers to run out. An answer that was on
  its way when the request came is not kept. The proxy keeps the last 256 requests for one player;
  past that, a session that did not see them forgets everything it kept, which errs on the side of
  asking again.
- **Counted** in `meridian_extension_orders_total{extension,method,outcome}`, where the outcome is
  `done`, `invalid`, `failed`, `budget`, `busy` or `refused`. A method the proxy does not have is
  counted under `method="unknown"`, never under the name the extension gave.
- **Configuration.** `[extension.orders]` with `rate-per-second` (20), `burst` (50), `max-running`
  (8) and `max-kicks-per-minute` (0, no limit), accepted when `grant` holds an `orders.*` or `decide.command`. Changing them closes the
  connection and opens it again, as changing any setting of the connection does.

## 6. Events: the proxy tells the extension (version 1.1, optional)

With the `events` capability, the proxy sends JSON-RPC notifications (no id, no answer), named as
Horizon names its own (`minecraft:notification/players/joined` in Horizon `management.rs:119`):

| Notification | When | Where it starts in the code |
| --- | --- | --- |
| `meridian:notification/players/joined` | A session opens on a server | `session::place`, once the session is listed |
| `meridian:notification/players/left` | A session ends | the guard of the session (`Leaving`), when it is let go of |
| `meridian:notification/players/moved` | A switch ends (seamless or failed, and why) | `switching::switch` |
| `meridian:notification/servers/up`, `.../servers/down` | The sidecar's status of a server comes back or fails | `status::spawn` (the round that reads the servers), `status::tell` |
| `meridian:notification/players/fallback` | A player is moved off a lost server | `fallback::replacer` |
| `meridian:notification/queue/entered`, `.../queue/left` (with the cause) | A player enters or leaves the line | `queue::Line::tell_entered`, `queue::Line::tell_left` |

- They go through a bounded queue for each extension, with one writer thread, so that no player's
  thread waits on a slow extension. Horizon's management protocol does the same for its
  notifications: a queue and a thread, so that no caller waits on a slow client (Horizon
  `crates/horizon-server/src/management.rs:27-31`, `Clients`).
- When the queue is full, the newest are dropped and counted, and the first event to go out after the
  loss is preceded by `meridian:notification/events/dropped` with the size of the gap. An extension that
  needs to be exact asks `players/list` again.
- Events are never a decision. An extension that wants to refuse something uses a decision
  point.

**As built, for the events** (`extension/events.rs`, `meridian-extension/src/events.rs`, `meridian-extension/src/serve/events.rs`).
Every notification of the table is built, and `events/dropped`.

- **Who is told.** Only a connection whose hello asked for `events` **and** that the operator granted it (`grant = ["events", ...]`), as for any
  capability. The SDK's `on_event` asks for it. A player's **address** is in `joined` only for a connection that asked for `addresses` and was
  granted it (decision Q9); it is the IP, with no port. Nothing else of a player is there: no packet, no chat, no key, and no secret of the
  proxy, of an extension or of a sidecar. Every word that says why (`reason`) is one of a fixed few, never a text a player wrote.
- **Shapes** (JSON-RPC notifications, no id, `params` an object). Every event has `seq` (the number of the event **on this connection**: 1 for the
  first of each connection, up by one for every event the proxy **tried** to send, the dropped ones included, so **a gap is a loss**) and `at` (milliseconds
  since the Unix epoch, by the proxy's clock). `player` is `{"name", "uuid", "auth": "verified"|"unverified"}`.
  - `meridian:notification/players/joined`: `{seq, at, player, server, address?}`.
  - `meridian:notification/players/left`: `{seq, at, player, server, reason}`, where `server` is the one the player was on when the session ended and `reason`
    is `left` (the player or the connection ended it), `kicked` (an operator, the console, a ban or an extension), `replaced` (the same player logged in again
    with `on-duplicate = "replace"`), `shutdown` (the proxy was stopping) or `error` (the session broke). The proxy marks a session it ends itself just before
    it disconnects the player; the others are `left` or `error` by how the relay ended. A reason this version of the SDK does not know is read as `other`.
  - `meridian:notification/players/moved`: `{seq, at, player, from, to, outcome, reason?}`. It is told for **every switch the proxy tried**, whoever asked for it (the console's `switch`
    and `send`, `/server` and `/hub` in game, the orders `players/send` and `servers/drain` with `move`, and the evacuation of a server that is going down): a switch is **tried** when `to` is a server
    of the proxy other than the one the player is on; a name that is no server, or the server the player is on, is not a switch and is not told, so what a player typed is never in an event. `outcome` is
    `seamless` (the player moved without leaving the game) or `failed` (the player stays where they were), and `reason` is there for a failure only: the step it failed at, in one word of the
    metrics of the transfers, `check` (before anything was asked of a server: no sidecar, a sidecar that cannot be reached, a switch already under way), `prepare`, `freeze`, `hand_over` or
    `commit`. **Nothing the servers said is in it.** The word `cold` of the first table of this section is not sent: a switch is seamless, and the other way a player changes server is a fallback.
    A word this version does not know is read as `other` (`outcome`) or `Other` (`reason`). `player.auth` is read from the live sessions when the switch ends: **a player who disconnected during the switch is
    told `unverified`**, whatever their account was (in `players/fallback` it comes from the session itself, which is still there).
  - `meridian:notification/players/fallback`: `{seq, at, player, from, to?, loss}`: the server `from` was lost for the player, `loss` is `crashed` (the connection closed or broke) or
    `kicked` (the server disconnected them), and `to` is the server the proxy **chose** for them; it is left out when no server could take the player, who is then disconnected (`players/left` follows).
    It is told when the proxy chooses, **before** the player's client follows: a client that does not follow ends the session (`left`). **A follow that succeeds is told by no event**: it does not go through a switch, so there is
    no `players/moved` for it, and no `left`; after a `players/fallback` with a `to`, the player is on that server unless a `players/left` comes or a new `players/fallback` of the same player does (if the server they arrived on is lost in its turn, the proxy chooses again and `from` is that server). What the server said when it kicked is not in it.
  - `meridian:notification/servers/down` and `meridian:notification/servers/up`: `{seq, at, server, cause?}` and `{seq, at, server}`. A server is **up** when its sidecar answers the proxy's
    probe (a sidecar that says it does not know the request is alive, so it answers) and **down** when it does not: the same reading the restart detector makes of each round. It is told **only
    when the state changes**: the first round that reads a server sets the state it starts from and tells nothing (a server that is down at the first round is told when it comes up), and a server
    that stays down is told once. `cause` is on a `down` only: `restart` when the restart detector already has the server going down for a restart at that moment, which is usually because it **announced its stop** (a server that goes silent without saying so is told `unreachable` at the first round it does not
    answer: the detector only counts it as restarting after `[restart] silent-rounds` rounds, 3 by default, and the `down` is not told again then) and
    `unreachable` when nothing says it is a restart; an `up` has none. **Only the servers with a role and a sidecar are watched, and only while `[balancing] enabled` is on**: that is the probe;
    a server that a reload removes is forgotten, and when it comes back its first reading is its base again. The name of the server is all that is in it: no address, no answer of the sidecar.
  - `meridian:notification/queue/entered`: `{seq, at, player, position}`: the player entered the line that waits for a place (`[queue]`), and `position` is the place they took, 1 for the first to be served;
    it is **their place when they entered**, and it is not told again as the line moves. A player who finds a place at once, or whom the line is too long for, never enters it and is not told.
  - `meridian:notification/queue/left`: `{seq, at, player, cause}`, told once for each `entered`: `served` (they got a place and a server), `left` (they closed the connection while they waited), `timeout` (they waited as
    long as they could), `shutdown` (the proxy was stopping) or `refused` (their turn came and they did not get a session: the login was refused or failed after the wait, which includes a connection that broke at that
    moment). A word this version does not know is read as `other`.
  - `meridian:notification/events/dropped`: `{at, count}`, with no `seq`.
  The proxy tells an event **after** the session is listed (`joined`) and **after** it is let go of (`left`): an extension that asks `players/list` on `left`
  finds the player gone. `left` is told by the session's guard, so it is told once **even if the relay panics** (the reason is then `error`). Fields are optional and a reader ignores one it does not know: **protocol 0, no change of version** when one is added.
- **What is guaranteed.** At most once, in order for one connection, **never replayed**. Each connection has a queue of its own (`[extension.events] queue`, 256
  by default, 1 to 10 000; an event is under 1 KiB) and **a thread of its own** that writes it, so that an extension that does not read holds that thread and nothing
  else: **no thread of a player and no queue of the decisions waits for it** (but see the socket, below). When the queue is full **the newest event is dropped**, counted
  (`meridian_extension_events_dropped_total{extension}`), and the gap is told **where it is**: the writer keeps the `seq` of the last event it wrote, and before it writes an event whose `seq` is not
  the one after it, it writes `events/dropped` with `count` = the size of the gap, so the notice comes exactly between the last event kept and the first one after the loss (`seq` is taken for every
  event offered, the dropped ones included). If no event follows the loss, the extension does not learn of it: **it asks `players/list` again** (`orders.read`) after an `events/dropped`, and after
  each connection, because **nothing is replayed**: what was queued is thrown away with its connection, and nothing is queued while the extension is not connected.
  **The events and the decisions share one socket and its write lock.** The events have a queue and a thread of their own, but a write that blocks (an extension that does not read) holds that lock for
  the write timeout, **5 seconds**, and a decision or an answer to an order that wants to write waits for it; at the timeout the connection is closed (as any write that fails closes it). So an extension
  that reads nothing holds up the decisions it is asked for, for at most 5 seconds, and then loses its connection; the on-error policies of the decisions apply meanwhile. Two events about different players have no order between them; for one player `joined` comes first and `left` last, with one known exception: a move that ends at the
  very moment the player leaves can be told after `left` (a `players/moved` event). Of the line, `entered` comes before `left` for one player; and the places in `entered` are the ones the players took, whatever
  the order in which two players at once are told.
- **The cost.** An event is a change of state of a session, never traffic: **nothing is on the path of the packets**. With no extension listening, an emission is **one relaxed
  atomic load** and the event is not even built (it is described by a closure that is not called). With listeners it is made once and handed to each queue with a `try_send`, which never
  waits, **outside any lock of the sessions**. `cargo test -p meridian-proxy -- --ignored --nocapture the_cost_of_an_event` measures an emission with and without a listener.
- **On the SDK's side** (`Extension::on_event`): the handler **never runs on the thread that reads the connection**. Events go through a queue of 1024 and a thread of their own; a slow handler
  delays nothing the proxy asks. When that queue is full the newest event is dropped and the handler gets an `Event::Dropped` before the next one (the same event as the proxy's own, with the
  SDK's clock in `at`). The SDK numbers each event as it offers it to its queue, and the thread tells the gap the same way the proxy does: before an event whose number is not the one after the last it
  handed over, a local `Event::Dropped` with the size of the gap (so, again, where the loss is, and only once an event follows it). A handler that panics loses that event and not the thread. `Event::parse` never
  fails: a method the SDK does not know, or params that are not its shape, is `Event::Unknown`, **with the name of the method cleaned of control and direction characters and cut to 64 characters**, since a
  program may log it.
- **Metrics**: `meridian_extension_events_total{extension,event}` (written) and `meridian_extension_events_dropped_total{extension}`.

## 7. Security, data and logs

**Who is trusted.** An extension holds the token the operator gave it and the capabilities the
operator granted. It is as trusted as those capabilities make it, and no more. A program on the
same machine without the token can open the port but learns nothing (section 2.2).

**What an extension can see:**

- names, unique ids and how they were checked (`auth`);
- the server each player is on, the servers, their roles, caps, counts and states;
- the host name a player typed;
- the commands players type that the proxy answers, with their arguments: the proxy's own ones, with
  `decide.command`, and, with `commands`, the extension's own. **The extension's own can be a word that is
  a command of the servers** (`/msg`, `/tell`...): the proxy takes it and never passes it on, so an extension
  granted `commands` sees what a player typed after it. The proxy keeps no list of words from an extension
  (the operator decides whom to grant `commands`, and its token is an administrator's credential). Of
  what goes to a server, an extension sees nothing but that;
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
  - `meridian_extension_decisions_total{extension,point,decision}`, where the point is `login`, `command` or `server` and the
    decision is what became of it (sections 4.2, 4.3 and 4.4);
  - `meridian_extension_breaker_open{extension}`;
  - `meridian_extension_orders_total{extension,method,outcome}`;
  - `meridian_extension_events_total{extension,event}`;
  - `meridian_extension_events_dropped_total{extension}`;
  - `meridian_extension_late_answers_total{extension}`, the answers that came after their request was given up on and were dropped.

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

**As built, for the orders that read** (`extension/orders.rs`, `extension/facts.rs`, `meridian-extension/src/orders.rs`):
`players/list`, `players/find`, `servers/list` and `queue/list`, all of capability `orders.read`, each costing one order of
the budget and one place while it runs, like any order. They read the proxy's own state when asked (the sessions, the
configuration in force, the balancer, the queue); until the proxy has given the registry its facts they answer
`-32603 not available`.

- **`players/list`**: params `{"limit": 1..500 (100 by default), "cursor": "<id>"}`, all optional. It answers
  `{"players": [...], "total": N, "next": "<id>"}`. The players are told **in the order of their unique ids**, and the
  `next` of a page, which is present only when more follow, is the id of the last player of it: players who join or
  leave between two pages make nobody skipped or told twice **among the players who are there at the cursor** (a place number would); a player who joins with an id before the cursor is not in the pages that follow. A `limit` over 500, 0 or not a number, a
  cursor that is not an id and a key nobody knows are `-32602`. 500 players of the longest the game allows, with an address,
  are a little over 100 KiB: inside the 256 KiB of a message (decision Q4: pages, the limit stays 256 KiB everywhere).
- **A player** is `{name, uuid, auth, server, seconds}`: the name, the unique id in lower case with its hyphens, how the
  proxy knows them (`auth`: `verified` or `unverified`: **for an unverified player the name and the id prove nothing**), the
  server they are on and how long they have been connected. **Their `address` is there only if the extension was asked for
  `addresses` and the operator granted it** (decision Q9); without it the key is absent.
- **Orders that take none.** `servers/list` and `queue/list` take no params: none, `null` and `{}` are fine, and anything
  else is `-32602 this order takes no parameters`, so that a program that thinks it is filtering finds out. `players/list`
  reads a `limit` or a `cursor` that is `null` as if it were left out.
- **`players/find`**: params `{"player": "<name or id>"}`. A text that is a unique id (with or without hyphens, in any
  case) is looked up by id, any other that is 1 to 32 characters with no control character by name, whatever its case.
  It answers `{"player": {...}}`, or `{"player": null}` when nobody online is that: not an error. Anything else is `-32602`.
  **Two players whose names differ only in case** (offline mode allows it) are not told apart by a name: the first the
  proxy finds is answered, and which is the first is not defined; the unique id is.
- **The address is given by the facts only when asked for**: the source of facts takes `with_addresses` and leaves the key out
  otherwise, so that a caller's mistake cannot tell an address to an extension that was not granted `addresses`.
- **`servers/list`** answers `{"servers": [...]}` in the order of the configuration. A server is
  `{name, default, players, role?, soft_cap?, hard_cap?, state?, ping_ms?}`: `role`, the caps and `state` (`open`,
  `closed`, `draining`, `not_offered`) only for a server the balancer places players on, `ping_ms` when its sidecar has been
  asked. **No address of a server is told**, with or without `addresses`. It asks no server anything: it is what the proxy
  knows (the console's `servers` also opens a connection to each one).
- **`queue/list`** answers `{"enabled": bool, "total": N, "waiting": [{position, name, seconds}, ...]}`: how many wait in all,
  and the first 500, the one who has waited longest first.
- **Cost.** One order each (the budget of 5). A program that wants every player on a large network pays one order for each
  500, not one for each player. **The SDK's `Proxy::players()` does this**, page after page, and gives up (`BadAnswer`) on a
  proxy whose cursors do not go forward or whose pages do not end.

**As built, for the orders that operate** (`extension/orders.rs`, `extension/operate.rs`, `meridian-extension/src/orders.rs`):
`servers/drain`, `servers/undrain` and `reload`, capability `orders.operate`. They are the console's `drain`, `undrain` and `reload`:
the same calls, so what an operator can do at the console an extension granted `orders.operate` can do over its connection.

- **`servers/drain`**: `{"server": "<name>", "move": true|false}` (`move` optional, `false` by default). It stops offering the server
  to new players (the balancer's drain, kept when the restart detector's own drain ends, as the console does) and answers
  `{"drained": bool, "moving": N}`: whether it was drained **now** (`false`: drained already, harmless), and, with `move`, how many
  players the proxy **started** to send to other servers of its role, one at a time on a thread of their own (the answer comes when
  the moves start). A server with no role (the balancer places players only on those) is `-32602 "server" is not a server with a
  role`: there is nothing to drain.
- **Cost, and what is paid first.** A drain without `move` costs one order. **A drain with `move` costs the number of players it
  sends, at most the burst, charged all at once, and takes the extension's place for a send** (one lot of moves at a time, as for
  `players/send`: another is `-32002 a send is already running`). The players on the server are counted and the budget pays
  **before anything is drained**: a drain that the budget cannot pay is `-32001` and drains nothing, so a server is never left
  drained with its players on it for want of budget. **Once the server is drained the order has done what matters**: if the moves
  cannot be started (no thread) it does not fail, it answers `{"drained": true, "moving": 0}` and the proxy's log says `could not
  start the moves`; look where the players are.
- **`servers/undrain`**: `{"server": "<name>"}`; answers `{"undrained": bool}`: `true` when it was drained and is offered again now,
  `false` when it was not drained (not an error). **A server that the restart detector is draining for a restart is not offered
  again by an extension**: `-32602 "server" is draining for a restart`, and it is offered when it is back. The console's own
  `undrain` is not bound by this: the operator's word is taken.
- **`reload`**: no params. **It answers `{"ok": true}` first and applies afterwards** (decision Q6): the proxy reads its file on the
  thread of the order **once the answer is written**, because a reload can change this very extension's entry (new address, token, or
  disabled), which closes the connection the answer would go on. So `ok` says that the proxy took the order, **not that the reload
  worked**: a file that cannot be read, or that does not pass the checks, changes nothing in use, and only the proxy's log says so
  (`reload (asked by an extension): ...`). One reload at a time for an extension: while one runs, another is `-32002 a reload is
  already running` and costs one token. Parameters are refused (`-32602`). **A reload costs half the burst** (rounded up), charged at
  once, since it reads, checks and applies the whole configuration: one that the budget cannot pay is `-32001` and costs one token,
  and nothing is read for it. A reload that removes or changes this extension's own entry closes the connection **after** the answer
  is written: the thread of the order ends and the place for a reload is let go.
- Until the proxy has given the registry what to operate they answer `-32603 not available`. **`orders.operate` lets the extension take
  servers out of rotation and make the proxy read another configuration** (the file on disk, not one the extension sends): grant it to a
  program as trusted as the operator's console.

**As built, for the order that runs console lines** (`extension/orders.rs`, `extension/console.rs`, `meridian-extension/src/orders.rs`):
`console/execute`, capability `orders.console`.

> **Granting `orders.console` is giving the operator's console.** The extension can run any line the operator can type: ban and
> pardon, change permissions and the whitelist, kick (the `meridian.kick.exempt` node does not apply to the console), drain,
> send, reload, and `end`, which stops the proxy. Grant it only to a program you would give the keyboard of the proxy to.
>
> **It also reads more than the read orders do:** `list` prints every player's address and `servers` every server's address (and
> `servers` opens a probe connection to each server at each call), **with or without `addresses`**. The most dangerous lines are `end`,
> `perm`, `ban` and `whitelist`, `kick` (which the `meridian.kick.exempt` node does not protect), `drain --move` and `send all`.

- **Params and answer**: `{"line": "<a console line>"}` answers `{"lines": ["..."], "truncated": false}`: what the console said,
  one string for each line. The line is one line of 1 to 1024 characters with no control, formatting (`§`) or invisible character
  (a line break would make two commands of one): anything else is `-32602` in fixed words, and nothing is logged or run.
- **Exactly the commands of the real console.** `main.rs` builds the table of commands once (`Commands::builtin()`) and gives
  **the same table** to the console that reads standard input and to the extensions: `Commands::execute`, with the same
  functions. There is no command more and none less: `help`, `list`, `servers`, `drain`, `undrain`, `switch`, `send`, `kick`,
  `ban`, `pardon`, `banlist`, `whitelist`, `alert`, `perm`, `queue`, `reload` and `end`, as they are at the time, and a word that
  is not in the table is refused as the console refuses it (`unknown command ...; type help for the list`; that is an answer, not
  an error). The console has no permission check of its own: it is the operator's, and so is this.
- **Granted by its name, never implied.** `grant` takes the names of the table of capabilities and nothing else: no pattern
  (`orders.*`, `*`), no abbreviation, and no other capability gives it (`orders.operate` does not, nor `decide.command`). It is off
  unless the operator writes `orders.console` in the `grant` of that extension **and** the extension asks for it in its hello. Without it
  the method is refused as if it did not exist (`-32601`), and nothing is logged or run.
- **Logged before, and after.** The proxy writes `extension "<name>" runs: <line>` in its log **before** it runs the line (the whole
  line: it cannot hold a line break or a control character), and, when it has ended, `extension "<name>" ran: <line>: said: <the
  first line> (and N more lines)` or `no output`. A line that never ends is in the log as `runs` only.
- **Every line costs half the burst, and one runs at a time.** Whatever the line says, it costs **half the burst** (rounded up),
  charged at once as the order `reload` is: `send all`, `alert` and `drain --move` are not cheaper through the console than through the
  orders that do the same. A line that the budget cannot pay is `-32001` (and costs one token) and **nothing is journaled or run**. **The
  budget limits how often, not what: `orders.console` is not bounded by it.** While one line runs, another from the same extension is
  `-32002 a command is already running` (and costs its token). The line runs on a thread of its own, **the only one the proxy
  starts for an extension's lines** (there is no other bound than that one); the extension's place for a send of players (`players/send`,
  `servers/drain` with `move`) is a different one, so a lot of moves and a console line can run at the same time. The line runs on that thread; if it has not ended after **four seconds** (less than
  the five an SDK waits by default) the answer is `-32603 did not finish`: **the line goes on running**, the place for a line stays held
  until it ends, and the log says `ran` when it does. A program reads that as "it may have run" (`may_have_run()`) and looks before it
  asks again.
- **The output is plain and bounded.** What a line may not hold (control characters, `§`, the invisible marks) is dropped from each
  line of output; at most **8192 characters in all** are given back (an empty line counts as one); past that the line that crosses
  the limit is cut, the next ones are left out, the last line is `[output truncated]` and `truncated` is `true`.
- **`end` and `reload`, which can cut the branch the extension sits on.** `end` stops the proxy, as Ctrl+C does: the line answers
  `stopping`, the proxy then closes the connections as it stops, and the answer is usually, not always, written before that. `reload`
  (the whole line, in any case) is answered **first**, as the order `reload` is (decision Q6): the answer says `reload asked: the proxy
  applies it after this answer; its log says what it did`, and the proxy reads its file once it is written, because a reload can change
  or remove this very extension's entry and close the connection. It holds the extension's place for a reload and pays
  the price of a line (half the burst) **once**, which is the price of the order `reload`: it does not pay twice. A `reload` with words after it is the console's usage refusal, and runs at once.
- Until the proxy has given the registry its console it answers `-32603 not available`.

**As built, for the order that expels** (`extension/orders.rs`, `extension/actions.rs`, `meridian-extension/src/orders.rs`):
`players/kick`, capability `orders.moderate`.

- **Params**: `{"player": "<name or id>", "reason": "<text>"}`, `reason` optional ("Kicked by an extension" when left out). It
  ends the session of the player, as the console's `kick` does (`Sessions::kick`), and answers `{"kicked": 1}`, or
  `{"kicked": 0}` when nobody online is that (not an error). The player is found as `players/find` finds them (a unique id with
  or without hyphens, or a name in any case). **`kicked: 1` says that a session was found and closed by the order**: a player
  who was already leaving counts as well, and a kick is not always instant (during a transfer the proxy may wait for the
  session's lock to end it). **Probing with `kicked` kicks the player if they are online**: only `0` is harmless.
- **A name two accounts have is refused.** If more than one player has that name without regard to case (a verified `alex`
  beside an unverified `Alex`, which can happen in `hybrid` mode: `on-duplicate = "refuse"` refuses a second login of the *same
  sort* of account, so it does not make names unique across the two), nobody is kicked and the answer is `-32602 more than one
  player has that name: use the unique id`; the unique id names one account. (During `on-duplicate = "replace"` the old session
  is listed until it ends, but it is the same account and both are ended.) The console's own `kick` is not changed by this.
- **The reason is shown as a literal**, exactly as written, never as a JSON text component: no click event, colour or hover. It
  is valid as the line of an extension's own command is (4.3): 1 to 256 characters with no control, formatting (`§`) or
  invisible character; anything else is `-32602` and nobody is kicked. A session that has not finished its configuration is
  ended too: the player reads the reason in the phase their client reads.
- **It costs one order**, and by default there is no other limit: not per player, not per second beyond the budget (20 a
  second with bursts of 50). **`orders.moderate` lets the extension expel any player, an operator's included, unless they are
  protected** (below): grant it to a program that is as trusted as an operator with the console's `kick`. Each kick is logged:
  `extension "<name>" kicked <player>: <reason>`.
- **Protecting players, off by default.** (a) A player who has the permission `meridian.kick.exempt` (by a group, a rank of their
  own, or by being an operator of the server they are on, as for any node) **cannot be kicked by an extension**: `-32602 that
  player cannot be kicked by an extension`, and nothing else is said. It is read at each kick, not when the player joined, and
  nothing gives it by default (a group that allows `*` has it). The console's `kick` is not affected. (b) `[extension.orders]
  max-kicks-per-minute` (0, no limit, by default): once an extension has kicked that many players in the last minute another
  kick is `-32002 too many kicks` and kicks nobody. Only a kick that closed a session counts: a player who is not online, one
  who is protected or a name that is refused does not use a place.
- Not the same twice, in a mild way: a player who was kicked is not online to be kicked again (the answer is `0`), unless they
  came back in the meantime. `orders.moderate` also tells the extension whether a name is online, as `orders.message` does.
- Until the proxy has given the registry its actions it answers `-32603 not available`.

**As built, for the order that moves** (`extension/orders.rs`, `extension/actions.rs`, `meridian-extension/src/orders.rs`):
`players/send`, capability `orders.move`.

- **Params**: `{"to": "<server>"}` and **exactly one** of `"player": "<name or id>"` (a name in any case, or a unique id),
  `"all": true` (every player online) and `"from": "<server>"` (the players on that server). It answers
  `{"started": N}`: **how many players the proxy started to move**, those who were not on `to` already. `0` is not an error
  (nobody was there to move, or they were all there already). A server the proxy does not have, as `to` or as `from`, is
  `-32602` in fixed words (the name is not given back); so is a `to` and a way to say whom that are not as above.
- **The answer comes when the moves start, not when they end.** The moves go on after it, one after the other, on a thread
  of their own, as the staff's `/send` does (the same function, `staff::move_all`): each player is told they are being
  moved and where, and how each move ended comes as a `players/moved` event (section 6). `players/find`
  tells where a player is. A player whose move is under way is not moved twice at once. A repeated order moves only those
  who are not on `to` yet.
- **Cost** (decision Q7): the number of players it moves, **and at most the burst**, charged all at once as a broadcast is
  (above): a `send all` of 500 players costs the whole burst of 50 and can run; one that the budget cannot pay is `-32001`,
  moves nobody, and costs one token. A plan is made first (who would move) and started only once the budget has paid it.
- **One send at a time for each extension.** The moves of a lot go on, one after the other, on a thread of their own; while that
  thread runs, another `players/send` of the same extension is answered **`-32002 a send is already running`** (fixed words)
  and costs one token. The thread gives its place back when it has moved them all, **however it ends** (a panic included). So a
  program cannot start threads without end, and the lots are moved in the order they were asked. Ask again when the moves
  have ended (watch `players/find`).
- **A destination that takes no players now is refused when the send is planned**: a server the balancer drained, that has not
  reported lately (`not_offered`) or that does not accept transfers answers **`-32602 "to" is a server that takes no players
  now`** (a code of its own would have been `-32603`, which this page keeps for an order that failed while it ran), and nobody
  is moved: sending a whole network to it would tell each player that they could not be moved. A server with no role is not
  the balancer's, and is taken; a server over its soft cap (`closed`) is taken too, as the staff's `/send` takes it.
- **A plan cannot be started unpaid.** The budget's payment is a value (`Paid`) that only the handler's charge makes, and
  `MovePlan::start` asks for it together with the extension's place for a send.
- Until the proxy has given the registry its actions it answers `-32603 not available`. `orders.move` also tells the
  extension whether a name is online and which server names exist, as `orders.message` does.

**As built, for the orders that message** (`extension/orders.rs`, `extension/actions.rs`, `meridian-extension/src/orders.rs`):
`players/tell` and `broadcast`, capability `orders.message`.

- **`players/tell`**: params `{"player": "<name or id>", "text": "<line>"}`; it answers `{"told": 1}`, or `{"told": 0}` when nobody
  online is that (not an error). The player is found as `players/find` finds them (a unique id with or without hyphens, or a
  name in any case). **The line is shown as plain text, exactly as written**: the proxy writes it as a literal, never as a
  JSON text component, so a click event, a colour or a hover cannot come from it (`<b>`, `&` or `{"clickEvent": ...}` are only
  characters). It is valid as the line of an extension's own command is (4.3): 1 to 256 characters, none of a control,
  formatting (`§`) or invisible character (a line break among them, which ends nothing: send another line). Anything else
  is `-32602`, and nobody is told. `meridian_extension::orders::clean_text` makes a line valid for a program that would
  rather clean than be refused.
- **`broadcast`**: params `{"text": "<line>", "server": "<name>"}`, `server` optional (every player when left out). It shows
  the line as the proxy's own `/alert` does (the same `[Alert] ` start, the same function) and answers `{"told": N}`. A server
  the proxy does not have is `-32602` in fixed words (the name the extension gave is not given back).
- **Cost** (decision Q7). `players/tell` costs one order. **`broadcast` costs the number of players it reaches, and at most
  the burst** (`[extension.orders] burst`): one that reaches 500 players costs the whole burst of 50, not 500, which would
  have made it impossible to run. **It is charged all at once, or not at all**: the dispatcher only looks that a token is there,
  the handler works out how many players it reaches, and then the budget pays the whole of it in one step. Two broadcasts that
  each need the whole burst, sent together, cannot each take a part of it and both fail: one is paid, the other is `-32001`.
  A broadcast that the budget cannot pay is `-32001`, **nobody was shown anything**, and it costs one token anyway (if there
  is one), so that trying again and again to find a cost that fits is not free; it can be asked again. It is logged as
  `extension "<name>" alerted <server or every server> to N: <line>`. **There is no limit for a given player or server**:
  only the budget bounds how often a line is sent to the same people.
- **`told` counts the players who were shown the line**, not those who were found: a session that has not finished its
  configuration (a player who is still joining) reads no chat, and is not counted. The proxy's own `/alert` still counts the
  players it found.
- **What `orders.message` tells the extension besides.** `players/tell` answers 1 or 0, which tells whether a name is online,
  and `broadcast` with a `server` answers `-32602` for a server the proxy does not have, which tells which server names exist:
  **`orders.message` also tells the extension whether a name is online and which server names exist.** A program that must
  not learn that is not granted it.
- **Not the same twice.** Unlike `permissions/invalidate`, these orders show a line each time they run: after a timeout or an
  end of the connection (`may_have_run`) the line may have been shown, and asking again may show it twice. Whether that
  matters is for the extension.
- Until the proxy has given the registry its actions they answer `-32603 not available`.

**As built, for the orders** (`meridian-extension/src/serve/client.rs`). The trait sketch above puts a `&Proxy` in
the argument of each method; the SDK's handlers are closures registered one by one (`on_login`, `on_command`...)
and keep their signatures, so the handle is made apart and given to the closures that need it:
`let proxy = extension.proxy();` (also `Running::proxy()`), cloned freely and used from any thread. Until a proxy
is connected its orders end with `NotConnected`.

- **Events.** `Extension::on_event(|event| ...)` registers the handler of the events and asks for `events` (see section 6, "As built"); it is not a method of `Proxy`.
- **Methods.** `Proxy::console(line)` for the order that runs console lines (it needs `orders.console`: the operator's console; the
  line is checked as the proxy checks it and nothing is sent if it is not valid; the answer is `Executed { lines, truncated }`);
  `Proxy::drain(server, move_players)`, `undrain(server)` and `reload()` for the orders that operate (they need
  `orders.operate`; a server that is not a name is `Invalid` and nothing is sent; `reload`'s `Ok(())` is that the proxy took the
  order); `Proxy::kick(player, reason)` for the order that expels (it needs `orders.moderate`; a reason that is not valid is
  `Invalid` and nothing is sent); `Proxy::send_player(player, to)`, `send_all(to)` and `send_server(from, to)` for the order that moves (they need
  `orders.move`; a name that is not one is `Invalid` and nothing is sent); `Proxy::tell(player, text)` and `Proxy::broadcast(server, text)` for the orders that message (they need
  `orders.message`: `Extension::wants("orders.message")` and the operator's grant; a line that is not valid is `Invalid` and
  nothing is sent); `Proxy::players_page(limit, cursor)`, `players()`, `find_player(&str)`, `servers()` and `queue()` for the orders
  that read (they need `orders.read`: the extension asks for it with `Extension::wants("orders.read")`, and for the addresses
  with `wants("addresses")`; the proxy serves a capability only if the hello asked for it **and** the operator granted
  it); `Proxy::invalidate_player(&str)` and `Proxy::invalidate_all()`, for `permissions/invalidate`; the
  unique id is checked and given its one spelling before anything is sent (a text that is not an id is
  `Invalid`, and nothing goes out). The other orders come with their PRs.
- **Ids.** Each order has a number of its own, counted up for as long as the extension runs (**not** started
  again at a new connection, so that no answer of an older connection can be taken for a newer order). The
  proxy's numbers and the extension's are two sequences, as 2.1 says: an answer is told from a request by what
  it holds, and the proxy's request 1 and the extension's order 1 do not meet.
- **Waiting.** An order waits for its answer on the thread that gave it, up to `order_timeout` (5 s by
  default), and a handler may give an order: the thread that reads the connection is another, and is free to hand
  the answer over. At most 64 orders wait at once; one more is `TooManyWaiting` at once, not queued. An answer
  that comes after its order gave up, or for a number nobody used, is dropped.
- **End of the connection.** The orders that wait end with `Disconnected` **at once**, before the SDK waits
  for the requests that are running, and the next are `NotConnected` until a proxy connects again. Nothing is
  retried by the SDK: after `Timeout` or `Disconnected` the order may or may not have been done.
- **Errors.** `Refused(RpcError)` carries what the proxy answered, with `is_not_granted()` (`-32601`: the capability
  was not granted or not asked for) and `retry_later()` (`-32001` budget spent, `-32002` places taken, or too many
  waiting). **In `-32601`, `-32602`, `-32001` and `-32002` the order did not run**, so asking again is always safe.
  **`-32603` ("the order failed") is not like them**: the proxy's handler panicked or its thread could not start
  while the order ran, so it **may have been done**. `may_have_run()` is true for it, for a timeout, for a
  connection that ended or broke while the order waited, for a write that failed and for an answer that is not
  what the order defines. Asking again after these is harmless for an order that has the same effect when done
  twice (`permissions/invalidate`: forgetting twice is forgetting once), and for another order it is for the
  extension to look first. With no proxy connected (`NotConnected`) nothing was sent: ask again when it has
  connected. An extension that must have a rights change forgotten (5) asks again until it is answered, a
  bounded number of times. What the proxy said in a refusal is its own text (up to 256 KiB): `Display` cleans it
  of line breaks and control characters and cuts it to 200 characters.

- Each method runs on one of the SDK's threads. The SDK keeps the deadline. An answer that would
  arrive after the proxy's timeout is not sent, and the SDK logs that it was late.
- A panic in a method is caught per request (`catch_unwind`), answered as an internal error, and
  the extension goes on. The proxy then applies its on-error policy.
- The handshake, the proofs, the limits and the validation are the SDK's. An extension's code sees
  only typed requests.
- The wire types live in one module that the proxy uses too, so that the two cannot drift. The
  proxy depends on that module, not on the SDK's server.

### 8.2 Examples

**Built:** four programs in `crates/meridian-extension/examples/`: `login_gate` (the admission at login),
`server_chooser` (the choice of server), `command_ranks` (the permission of a command) and `command_party`
(an extension's own command). [EXTENSION-GUIDE.md](EXTENSION-GUIDE.md) walks through them. **Not built:** the two
below, `allow-all` and `file-permissions`, which this page planned and which are not in the repository.

- **`allow-all`** (not built). It answers `Default` to everything and logs each request with its time. It is
  the smallest extension that runs, the template to copy, and the one the proxy's integration
  tests start. It shows that the proxy with an extension that never disagrees behaves as without
  one.
- **`file-permissions`** (not built). It reads a TOML file of players (by unique id) and the nodes they are
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
10. **The SDK and `allow-all`** (the SDK is built; `allow-all` is not). Proof: the SDK's tests, and the proxy's integration test with
    `allow-all` started as a process on both systems.
11. **`file-permissions`** (not built). Proof: its tests, and an integration test of a rank given by the file.
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
  Mitigation: capabilities granted one by one and by name only, off by default, and `console/execute` logged before it runs,
  one line at a time, with a time limit and a bounded output.
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
