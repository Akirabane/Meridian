# Writing an extension

An extension is a program of your own that Meridian asks before it acts. Today it can **refuse a
player at login**; the other decision points (the permission of a command, the choice of server) are
specified in [EXTENSIONS.md](EXTENSIONS.md) and not built yet. This page takes you from nothing to a
working extension in Rust. Protocol 0 is experimental: anything in it may change between two releases.

## What happens

You start your program, which listens on a port of the loopback. The proxy connects to it, proves it
knows the same token as your program (and asks your program to prove it), and then, for each player
who has logged in and is not banned, asks your program whether to let them go on. Your program can say
no, with a reason the player reads. It can never say yes to a player the proxy's own bans or whitelist
turned away: it is asked after them, and it is not asked about those players at all.

If your program is down, slow or wrong, the proxy does what **you** chose in `on-error` (below), and by
default carries on as if there were no extension. A crash of your program does not take the proxy
down, and does not lock players out unless you ask it to.

## 1. A token

The token is a secret that the proxy and your program share: at least 32 bytes, in a file. Make one
that nobody else can read:

```sh
mkdir -p extensions
openssl rand -hex 32 > extensions/gate.token
chmod 600 extensions/gate.token
```

Both sides read the same file. A line end after the token is ignored. The token is never in
`meridian.toml`, never in a log and never in an error message; only the path of its file is.

## 2. The program

Add the crate to yours (it lives in the Meridian repository, `crates/meridian-extension`, and depends on
nothing but `serde`, `serde_json`, `getrandom` and Meridian's own crates):

```toml
[dependencies]
meridian-extension = { path = "../meridian/crates/meridian-extension" }
```

Then the whole program, shortened from `crates/meridian-extension/examples/login_gate.rs`, which takes
the token file, the address and the names to refuse on its command line and can be run as it is
(`cargo run -p meridian-extension --example login_gate -- extensions/gate.token 127.0.0.1:25590 Alex`):

```rust
use meridian_extension::decide::{Decision, LoginRequest};
use meridian_extension::handshake::Secret;
use meridian_extension::serve::Extension;

fn decide(blocked: &[String], request: &LoginRequest) -> Decision {
    let name = &request.player.name;
    if blocked.iter().any(|b| b.eq_ignore_ascii_case(name)) {
        Decision::deny("This account is not welcome here.")
    } else {
        Decision::Default
    }
}

fn main() {
    let token = Secret::from_file_contents(&std::fs::read("extensions/gate.token").unwrap()).unwrap();
    let blocked = vec!["Alex".to_owned()];
    Extension::new("login-gate", "1.0.0", token)
        .on_login(move |request| decide(&blocked, request))
        .serve("127.0.0.1:25590")
        .unwrap();
}
```

What the handler gets, a `LoginRequest`:

| Field | Meaning |
| --- | --- |
| `player.name`, `player.uuid` | What the player logged in with |
| `player.auth` | `Verified` if the session service confirmed the account; `Unverified` in offline mode, or for the unverified players of the hybrid mode. **For an unverified player the name and the id prove nothing**: anyone can type any name. Do not treat a name as an identity unless `auth` is `Verified` |
| `host` | The host name the player typed to reach the proxy, if any |
| `address` | The player's IP address, only if you ask for it and the operator allows it (see below) |
| `connected` | Whether a session of the same player is open now |

What it answers, a `Decision`:

| Answer | Effect |
| --- | --- |
| `Decision::deny("text")` | The player is refused with this text. At most 256 characters; control characters are dropped and the text is cut for you, so the proxy always accepts it |
| `Decision::Deny(None)` | Refused, with the proxy's own words ("You cannot join this server right now.") |
| `Decision::Allow` | No objection. The player goes on to the checks that follow (a second login, the player limit, the choice of server), and the proxy records that you looked |
| `Decision::Default` | The same, without the record: as if you had not been asked |

An extension can only **add refusals**. `Allow` is not a pass.

The handler runs on a thread of its own, one per request, at most `max-in-flight` at once (64 unless you
set `.max_in_flight(n)`). **The SDK has no time limit for a handler**: if yours waits for something (a
database, a web request), bound that wait yourself. The proxy gives up on its own at the deadline you
configure, and then applies `on-error`, but your handler keeps running and keeps its place until it
returns.

For the player's address, ask for it: `.wants("addresses")`. Nothing is sent to a program that did not.

`.serve(address)` runs until the process is killed. To stop cleanly from another thread, use
`.bind(address)`, which returns a handle with `stop()`. The address must be on the loopback
(`127.0.0.1` or `[::1]`); anything else is refused. The SDK writes what it does to the standard error
stream (`.log(...)` takes your own logger), and never a player's name, address or id, a message or the
token.

## 3. The proxy's configuration

In `meridian.toml`, one `[[extension]]` for each program ([CONFIG.md](CONFIG.md) lists every key):

```toml
[[extension]]
name = "gate"
address = "127.0.0.1:25590"
token-file = "extensions/gate.token"
grant = ["decide.login"]

[extension.decide-login]
timeout-ms = 1000
on-error = "default"      # default | allow | deny
```

- `grant` is what you allow the program to do. It asks for what it wants in its hello (`decide.login`
  comes with `.on_login`), and the proxy uses only what you granted and the program asked for.
- `timeout-ms` is how long a login waits for your answer. The client gives up after 30 seconds of
  silence, so this, `connect-timeout-ms` and the queue's wait must fit in 27 seconds; the proxy checks it
  when it reads the file.
- `on-error` is what happens when there is no usable answer (your program is not connected, is too
  slow, answers something invalid, or its breaker is open): `default` and `allow` let the player go on,
  `deny` refuses. The default is `default`.
- The configuration is read when the proxy starts and at `reload`, and the token file at each
  connection. A reload that changes the address, the token file, `grant` or the numbers of the
  `[[extension]]` connects again; one that changes only a `decide-login` value does not.
- To add `addresses`: `grant = ["decide.login", "addresses"]` and `.wants("addresses")` in the program.

## 4. Run it

Start your program and the proxy, in any order: the proxy connects when it can, and tries again with a
wait that doubles up to 30 seconds. The proxy's log says what it found:

```text
extension "gate": connected, protocol 0, "login-gate" 1.0.0, granted [decide.login], refused []
extension "gate": protocol 0 is experimental
```

If you grant `decide.login` and the program did not ask for it (it has no `.on_login`), the proxy says so
once at the hello: `extension "gate" is granted decide.login but did not declare it: its requests are
not sent to it`. Nothing is asked of that program, whatever its `on-error` says.

A wrong token is refused without a word to the other side. The proxy logs
`extension "gate": refused: wrong token; trying again` (and waits the longest before it tries again), and
your program logs `the hello failed: the proxy closed the connection during the hello (code 1008)`.
Check that both read the same file.

With `[metrics]` on, the proxy's page has `meridian_extension_connected{extension="gate"}`, the requests
by how they ended (`answered`, `timeout`, `invalid`, `busy`, `breaker`, `disconnected`), their time, and
`meridian_extension_decisions_total` by what became of each decision. A refusal counts as
`meridian_login_failures_total{reason="extension"}`.

## Good to know

- **One connection at a time.** The proxy opens one connection for each extension. A second one is told
  `503` and dropped, so run one copy of your program for each `[[extension]]`.
- **The breaker.** After 5 decisions in a row without a usable answer (`breaker-failures`) the proxy stops
  asking for 30 seconds (`breaker-seconds`) and every login takes `on-error`; then it tries one. A broken
  connection opens it at once, and the next hello closes it.
- **Messages** are at most 256 KiB and must arrive whole in five seconds from their first byte.
- **Versions.** Your program and the proxy agree on a protocol version in the hello; with none in common
  the proxy logs it and does not use the extension. Protocol 0 is experimental, so rebuild your program
  with the Meridian you run.
- **Testing.** `crates/meridian-extension/src/serve/tests.rs` and
  `crates/meridian-proxy/src/session/tests/extension.rs` talk to an extension the way the proxy does and
  show how to test one.
