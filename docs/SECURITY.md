# Security

This page says who the proxy expects to be attacked by, what it guarantees against them,
the limit it holds each input to, and what it does not cover. Every limit names the place in
the code that holds it.

## Reporting a vulnerability

Report a vulnerability privately, through the repository's private vulnerability reporting on
GitHub: the **Security** tab of the repository, then **Report a vulnerability**. Do not open a
public issue for a vulnerability. Say what an attacker needs (an address that reaches the
proxy, an account, access to the machine), what they get, and how to reproduce it. A report is
answered there, and the fix is released before the details are made public.

## Who attacks what

The proxy listens on the internet, and anyone may connect to it. From the most exposed to the
least:

1. **Anyone with an address that reaches the proxy's port.** They need no account. They can
   open connections, send any bytes before and during a login, send server list pings, and,
   when the proxy is in offline or hybrid mode, log in under any name that is not an account's.
   This is the attacker the limits below are written against first.
2. **A player who logged in.** In online mode they have a real account. They send play packets,
   some of which the proxy reads (commands it answers, positions it follows for a switch).
3. **Whatever can reach a sidecar's port.** The sidecar listens on 127.0.0.1 by default. It
   accepts another address only with `--allow-remote`, and then only without encryption with
   `--insecure-remote` as well. Its requests are signed with the shared secret, so without the
   secret a peer cannot make it act. It can still send it bytes.
4. **Whatever can reach the metrics page.** The page is off by default and binds 127.0.0.1
   when it is on. It has no password.
5. **The operator's own files and console.** The configuration, `access.toml`,
   `permissions.toml`, the favicon and the console are the operator's. They are checked so that
   a mistake is refused with a message, but they are not defended against someone who can write
   them.

## What the proxy guarantees

Against the first two, the proxy guarantees the following. Each item is tested.

- **A bounded time before a session.** A handshake and a login end within 30 s in all. A
  handshake, a status request and its ping end within 5 s. The client's next byte is never
  awaited for more than 10 s. These hold whatever pace the bytes come at: before encryption, the
  frames are read a byte at a time, and each read waits at most until the deadline.
- **Bounded memory before a session.** A frame longer than 2048 bytes is refused before the
  login is done, from its length, before any of it is kept. So is a compressed frame that says
  it inflates to more. The longest packet a client sends before its session is a handshake with
  a host of 255 characters, under 800 bytes.
- **A bounded number of connections.** One address opens at most `connections-per-minute`
  connections a minute (20 by default). At most `max-pending-logins` connections (2048 by
  default) are logging in at once, and optionally `max-pending-logins-per-address` from one
  address. A connection over a limit is closed before a byte is read.
- **No login past its rules.** A banned address is closed before a byte is read. A banned name
  or unique id, a name that breaks vanilla's rules (1 to 16 characters from 33 to 126), a
  protocol version other than the proxy's, a transfer, and an answer to a query the proxy never
  sent each end the login.
- **The forwarding secret stays with the proxy.** It is sent only to the servers, inside the
  signed player information, and never in a log line or debug output.
- **One event, one log line.** A log line may quote text a player or a peer chose (the message
  of `/alert`, an error their bytes caused). A line break, a carriage return, any other control
  character and the characters that turn text around are written escaped (`\n`, `\u{1b}`), so a
  quoted text can neither start a line that looks like the proxy's own nor move a terminal's
  cursor (`crates/meridian-proxy/src/log.rs`, `line`).
- **No panic from the wire.** Every reader of bytes that come from outside is fed thousands of
  changed inputs in the tests (see below), and none may panic.

## Limits, input by input

| Input | Limit | Where in the code |
| --- | --- | --- |
| New connections from one address | `connections-per-minute`, 20 a minute | `crates/meridian-proxy/src/throttle.rs` (`Throttle::allow`), checked in `serve.rs` before a thread starts |
| Connections logging in at once | `max-pending-logins`, 2048; `max-pending-logins-per-address`, off | `crates/meridian-proxy/src/throttle.rs` (`Logins::enter`), `serve.rs` (`enter_login`) |
| PROXY protocol header | Read only from trusted addresses; version 1 at most 107 bytes, version 2 at most 16 + 536; `header-timeout-ms` in all, 3 s | `crates/meridian-net/src/proxy_protocol.rs` (`V1_MAX`, `V2_MAX_PAYLOAD`, `read_header`) |
| The client's silence before its session | 10 s for each byte | `crates/meridian-net/src/client.rs` (`BOUNDS.silence`) |
| A handshake and a login | 30 s in all | `client.rs` (`BOUNDS.login`, `Early`) |
| A handshake and a status request | 5 s in all; one status answer and one pong | `client.rs` (`BOUNDS.status`, `serve_status`) |
| A frame before the session | 2048 bytes, and as much once inflated | `client.rs` (`BOUNDS.max_frame`, `Early::recv`) |
| A player's name | 1 to 16 characters, from 33 to 126 | `client.rs` (`valid_name`) |
| A frame after the login | 2 MiB, and 8 MiB once inflated | `horizon-protocol`, `frame.rs` (`MAX_FRAME`, `MAX_UNCOMPRESSED`) |
| The client's silence in a session | 30 s for each read | `client.rs` (`READ_TIMEOUT`) |
| Packets kept while a player waits in the queue | 64 packets and 1 MiB in all | `crates/meridian-proxy/src/queue.rs` (`MAX_EARLY_PACKETS`, `MAX_EARLY_BYTES`) |
| A wait in the queue | `wait-seconds`, at most 20 s | `crates/meridian-config/src/queue.rs` |
| A sidecar's request | Signed with HMAC-SHA256 over a challenge fresh for each connection and an increasing number, so a replay or a request from another connection is refused | `crates/meridian-sidecar/src/wire.rs` |
| A line between the proxy and a sidecar | 32 MiB and 4 KiB for a signed line, 256 bytes for the challenge, refused as soon as it passes the limit, before its signature is checked; 300 s of silence for each read | `crates/meridian-sidecar/src/wire.rs` (`read_line`, `MAX_SIGNED_LINE`, `MAX_CHALLENGE_LINE`), `main.rs` (`IDLE`) |
| An answer of a server's control endpoint | 16 MiB, the endpoint's own limit | `crates/meridian-sidecar/src/endpoint.rs` (`read_json`), `request.rs` (`MAX_LINE`) |
| A message of a server's management protocol | 256 KiB, fragments included; the answer to the upgrade 16 KiB | `crates/meridian-sidecar/src/websocket.rs` (`MAX_MESSAGE`, `MAX_HEAD`) |
| A request to the metrics page | 8 KiB; 4 s in all, 2 s for each read; 8 connections at once | `crates/meridian-proxy/src/metrics.rs` (`MAX_REQUEST`, `REQUEST_DEADLINE`, `IO_TIMEOUT`, `MAX_CONNECTIONS`) |
| The favicon | A 64 by 64 PNG of at most 16 KiB | `crates/meridian-config/src/favicon.rs` |

## How the readers are tested against hostile input

Each reader of outside bytes has tests that feed it what an attacker would: a VarInt too long
or negative, a length of zero, huge or past the end, a packet cut short, a string too long or
not UTF-8, a packet of the wrong state, a compressed size far above what follows, a WebSocket
frame of an unknown kind, unmasked, in endless fragments or with a lying length, a PROXY header
from an untrusted address, a sidecar line badly signed, replayed or too long, a request to the
metrics page that never ends, and connections that come slowly or in numbers.

Beside them, a mutation test runs on every reader: valid inputs are changed at random (a bit,
a byte at an edge, bytes cut, repeated or inserted, a length made extreme) from a fixed seed,
thousands of times, so that every run on every machine tries the same inputs. Each must be
answered without a panic, the player's side within its deadline, and with no more kept than its
limits allow. The changes are made by `crates/meridian-testkit/src/mutate.rs`, with no crate
added, and the tests are named `hostile_*`:

| Reader | Test |
| --- | --- |
| The handshake, the status and the login | `crates/meridian-net/src/client_fuzz_tests.rs` |
| The PROXY protocol | `crates/meridian-net/src/proxy_protocol/tests.rs` |
| The ghost and the delta of a switch | `crates/meridian-ghost/src/tests/fuzz.rs` |
| The rewriting of entity ids | `crates/meridian-rewrite/src/tests.rs` |
| The poses a switch carries | `crates/meridian-handoff/src/tests/hostile.rs` |
| A sidecar's signed lines | `crates/meridian-sidecar/src/wire.rs` |
| The management protocol's frames | `crates/meridian-sidecar/src/websocket.rs` |

They take well under a second each and run with `cargo test`. A fuzzer that follows coverage,
such as `cargo fuzz` (it needs a nightly compiler and `cargo install cargo-fuzz`), can be pointed
at the same functions for a longer search: `client::accept`, `proxy_protocol::parse_v1` and
`parse_v2`, `View::apply`, `Swap::clientbound` and `serverbound`, `wire::open` and
`Socket::read_message`. It is not part of the repository or of CI.

## What is not covered

- **The leg between the proxy and the servers is in clear.** After a signed login, the packets
  between the proxy and a server are plain TCP, as they are between a vanilla proxy and its
  servers. Keep the servers on a private network, or on the same machine.
- **The sidecar's connection to a server's management protocol does not check the certificate.**
  The sidecar reads only a server on its own machine, and refuses any other address. The TLS it
  uses there keeps the traffic from other processes' view, but proves nothing about who answers.
- **A remote sidecar is unencrypted.** With `--allow-remote --insecure-remote`, player records
  cross the network in clear. The signatures stop a stranger from making the sidecar act, but
  they do not hide what it says.
- **The console is trusted.** It is standard input, and whoever can type in it is the operator.
- **An online login after encryption.** Once encryption is on, the proxy reads through the
  connection of the protocol layer. The deadline is checked before each packet, and a frame is
  bounded at 2 MiB, but within one packet the bytes may come slowly. By then the player has been
  checked by the session service: they have an account, which the operator can ban.
- **A packet after the login may be large.** The protocol layer reads a frame of up to 2 MiB,
  and sets aside the 8 MiB a compressed one says it inflates to, before it knows whether the
  packet is valid. That is per connection and per packet, as a vanilla server allows, and the
  limits on the number of connections bound the total.
- **The operator's files are trusted.** The favicon is read whole before its size is checked,
  and the configuration, `access.toml` and `permissions.toml` are read whole too. Someone who
  can write them can do worse.
- **A sidecar takes any number of connections.** It listens on the loopback by default, and a
  connection that does not prove the secret makes it do nothing. Each one still costs a thread
  until its first line is refused or its silence of 300 s ends it.
- **Many addresses at once.** The limits above bound what one address costs, and how many
  connections log in at once. An attacker with many addresses can still fill
  `max-pending-logins` and keep new players waiting while they hold it. That is a matter for the
  network in front of the proxy (a firewall, a provider's protection), not for the proxy alone.
