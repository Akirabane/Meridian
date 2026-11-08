# Writing an extension

An extension is a program of your own that Meridian asks before it acts. Today it can **refuse a
player at login**, **choose the server a player goes to** (section 5), **decide who may use the proxy's
commands** (section 6) and **answer commands of its own** (section 7). This page takes you from nothing to a
working extension in Rust. Protocol 0 is
experimental: anything in it may change between two releases.

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

## 5. Choosing the server

The second decision point is where a player goes. When the proxy places a player, at login and when
their server is lost, it can show your program the servers the routes allow and let it put them in an
order of its own, or refuse the player. Register it next to the login, or alone:

```rust
use meridian_extension::decide::{Choice, ServerRequest};

fn choose(request: &ServerRequest) -> Choice {
    // The servers the player may be sent to, in the order the proxy would try them.
    let open: Vec<&str> = request
        .candidates
        .iter()
        .filter(|c| c.state.as_deref() != Some("draining"))
        .map(|c| c.name.as_str())
        .collect();
    if open.is_empty() {
        Choice::refuse("No game is running.")
    } else {
        Choice::order(open)
    }
}

// Extension::new("chooser", "1.0.0", token).on_server(choose).serve("127.0.0.1:25591")
```

The whole program is `crates/meridian-extension/examples/server_chooser.rs`; it takes the token file and the
address on its command line.

In `meridian.toml`, grant `decide.server` and, if you want, set its table:

```toml
[[extension]]
name = "chooser"
address = "127.0.0.1:25591"
token-file = "extensions/chooser.token"
grant = ["decide.server"]

[extension.decide-server]
timeout-ms = 1000
on-error = "default"      # default | refuse
```

What the handler gets, a `ServerRequest`: the `player` (with `auth`, and the same warning as at login:
an unverified name proves nothing), `reason` (`Placing::Login` or `Placing::Fallback`), `host`, `lost` (the
server the player lost, at the fallback), and the `candidates`. Each candidate has a `name`, and, when
balancing is on and the server has a role, its `role`, `players`, `soft_cap`, `hard_cap` and `state`
(`open`, `closed`, `draining` or `not_offered`); for the others those are `None`.

What it answers, a `Choice`:

| Answer | Effect |
| --- | --- |
| `Choice::Default` | The proxy goes on as it would without you: the routes, the health order and the balancer |
| `Choice::order([...])` | The proxy tries these servers in this order, and only these. **Each name must be a name of `request.candidates`**, spelled as it is, or the answer is invalid. A server that is full, drained or not heard from is passed over for the next one. The balancer is not asked, but the hard cap of each server holds |
| `Choice::refuse("text")`, `Choice::Refuse(None)` | The player is refused, with your text (as for a login) or the proxy's own words. At the fallback the player is disconnected with it |

An invalid answer (a name that was not offered, an empty order) counts against your breaker, and the
proxy takes the `on-error` policy: `default`, or `refuse` with its own words. So do a timeout, a program
that is not connected and an open breaker. At the fallback the wait is at most 500 ms whatever
`timeout-ms` says, since the player is waiting in the configuration phase, and **a failure there is
never a refusal**: the player is already in the game, so they are placed as without you, whatever
`on-error` says. Only a `Choice::refuse` you send yourself disconnects them.

An order may send a player to a server at its soft cap (`closed`), up to its hard cap: where players go
is your decision, and the hard cap always holds.

The decision is asked after the player has a place under `max-players`, and held there while you answer:
keep the handler quick. With the queue on, a player whose ordered servers are all full waits, and you are
asked again when they are woken.

## 6. Deciding who may use a command

The third decision point is the permission of the commands the proxy answers itself (`/server`, `/hub`,
`/play`, `/send`, `/glist`, `/alert`; the commands in `[commands] enabled`). When a player types one, the
proxy can ask your program which of the **nodes** of permission the command checks (`meridian.send`,
`meridian.server.east`, ...) the player has. Your program becomes the source of rights for these commands:

```rust
use meridian_extension::decide::{CommandRequest, NodeAnswers};

fn decide(request: &CommandRequest) -> NodeAnswers {
    if request.player.name == "Alex" {
        // Alex may do all of it, whatever permissions.toml says. Keep it for a minute.
        request.nodes.iter().fold(NodeAnswers::new(), |a, node| a.allow(node)).cache_seconds(60)
    } else {
        NodeAnswers::new().cache_seconds(60) // nothing to say: permissions.toml decides
    }
}

// Extension::new("ranks", "1.0.0", token).on_command(decide).serve("127.0.0.1:25592")
```

The whole program, with a list of administrators on its command line, is
`crates/meridian-extension/examples/command_ranks.rs`.

```toml
[[extension]]
name = "ranks"
address = "127.0.0.1:25592"
token-file = "extensions/ranks.token"
grant = ["decide.command"]

[extension.decide-command]
timeout-ms = 250
on-error = "default"      # default | deny
cache-seconds-max = 300
```

What the handler gets, a `CommandRequest`: the `player` (with `auth`: for an unverified player the name
proves nothing), the `server` they are on, the `command` and its `args` (what the player typed: at most 32
arguments of 256 characters, **not to be trusted**), and the `nodes` the command checks. What it answers, a
`NodeAnswers`:

| Answer | Effect |
| --- | --- |
| `.allow(node)` | The player has the node, **even if `permissions.toml` or the operators of the server refuse it** |
| `.deny(node)` | The player has not the node, even if `permissions.toml` gives it |
| a node left out | `default`: `permissions.toml` and the operators decide, as without an extension |
| `.cache_seconds(n)` | The proxy keeps these answers for this player for `n` seconds, and no longer than `cache-seconds-max`. They are kept for the same command with the same arguments on the same server, and not for another |

**Read this before you grant it:**

- **Only an extension you grant `decide.command` to decides.** One that is not granted it, is switched off
  or did not declare it is never asked.
- **Your `allow` can give `/send`, `/alert` and the other commands to any player. The token of this
  program is therefore an administrator's credential for the network: keep its file readable by the proxy
  and by the program only.** Whoever holds it can make the proxy treat anyone as an administrator.
- **It concerns the proxy's commands only.** Any other command goes to the server as before, and the
  login keeps its own order: the bans and the whitelist come first and no extension bypasses them.
- **`cache-seconds-max` bounds how long an `allow` or a `deny` is kept**, whatever you ask for. What is kept
  belongs to the player's session, and is dropped at a reload, when your program says hello again and when
  the player changes server. **Only give `cache_seconds` to a verdict that depends on what the request
  carries** (the server, the command, its arguments, the node): one that depends on the time of day or on
  a rank you change meanwhile would be kept as long as you said.
- **`on-error = "allow"` is refused for this point, when the proxy reads its configuration**: it would give
  every node to whoever types the command whenever your program does not answer (down, too slow, an
  invalid answer, an open breaker), so `/send` and `/alert` would run for everyone. Use `"default"` (the
  file decides, the default) or `"deny"`. Nothing that fails ever grants a node.

The player's packets to the server wait while your program answers, so the wait is short (250 ms by
default) and the proxy asks only for a command it answers, once for all the nodes the command checks, and
not at all for what it kept. Five misses in a row open the breaker, and then commands take `on-error` at
once. Keep the handler quick, and let the proxy keep what does not change.

## 7. Commands of your own

Your program can have commands that the proxy takes from the player and **never passes to the server**, as
it does for `/server` and `/hub`. The player types `/party invite Bob`, the proxy asks your program, and
the lines it answers appear in the player's chat when they come (`crates/meridian-extension/examples/command_party.rs`
is a small one). It needs the `commands` capability:

```toml
[[extension]]
name = "parties"
address = "127.0.0.1:25593"
token-file = "extensions/parties.token"
grant = ["commands"]

[extension.commands]
timeout-ms = 2000        # how long the lines may take, 1 to 10000
```

An extension granted `commands` needs `max-in-flight` of at least 2: its commands use at most half of its
places, so that its login and permission decisions keep one.

```rust
let extension = Extension::new("parties", env!("CARGO_PKG_VERSION"), token)
    .command(
        CommandSpec { name: "party".into(), aliases: vec!["p".into()], usage: "/party <invite|leave>".into() },
        |request| match request.args.as_slice() {
            [word, who] if word == "invite" => Lines::one(&format!("{who} is invited to your party.")),
            _ => Lines::one("Usage: /party <invite|leave> [player]"),
        },
    );
```

- **The words.** A command and its aliases are 1 to 32 characters among `a-z`, `0-9`, `-` and `_`; a command has
  at most 8 aliases and a `usage` of at most 256 characters, and an extension at most 64 commands. The SDK
  panics on one that is not valid, is declared twice or is one too many. A word that is a command of the proxy that is
  enabled in `[commands]` (`/lobby` is a word of `/hub`) is refused and logged, as is a word that an
  extension earlier in the configuration has: the first keeps it. The proxy reads your declaration at each
  hello, so a new command needs your program to start again.
- **The request** has the command's declared name (not the alias the player typed), the arguments **as the
  player typed them: do not trust them**, who the player is (`auth` says whether the account was checked), and
  the server they are on. At most 32 arguments, each cut to 256 characters.
- **The answer** is at most 10 lines of 256 characters, plain text: `Lines::new` cuts and cleans what the
  proxy would refuse. It is never read as a text component, so a line cannot hold a click event. **A line has
  no `§` (the client reads it as a colour or a flicker), no control character, and no mark that reverses or
  hides text (direction marks, characters of no width, invisible fillers, variation selectors, tag
  characters; `run::is_refused` lists them): `Lines::new` drops them, and the proxy refuses an answer that
  has one.** **Known limit:** dropping the zero-width non-joiner and joiner (U+200C, U+200D) breaks the
  separation of letters in Persian and some Indic scripts, and dropping the direction marks (U+200E, U+200F)
  can change how Arabic or Hebrew mixed with Latin text reads: such text needs the field that would relax this
  rule, which does not exist yet. This is a prudent default: colours would need a field of their own, that the
  operator allows, added later. A command with nothing to say answers `Lines::none()`.
- **The permission** is the node `ext.<extension>.<command>` (the extension's name in the configuration, in
  lower case), **open to everyone** unless `permissions.toml` closes it (`deny = ["ext.parties.party"]`).
  The extension that holds `decide.command` is asked about it only if it is **the extension that owns the
  command**: another one is not told what a player typed after your command.
- **One command at a time for a player, and half of your places at most.** A second command typed while the
  first still runs reads `Still working on your last command.`, and when half of your `max-in-flight` places
  already run commands, the next player reads `That command did not answer.` at once. So players cannot take
  the places your login and permission decisions need.
- **When it fails** (your program is down, slower than `timeout-ms`, or answers something invalid) the
  player reads `That command did not answer.`. A command counts **neither for nor against the breaker**
  (one slow command does not turn every login to its on-error policy, and an answer to a command does not
  hide that your logins time out), but a breaker that is open stops the call. **For now** a command stays
  yours while your program is down (the player reads that line, and the server never sees the word); this
  may change.
- **A word can be a command of the server.** The proxy takes any word that is not a command of the proxy
  enabled in `[commands]`, and never passes it to a server: a `/msg` of yours would be valid on the whole
  network, and also receive everything a player types after it. There is no list of words the proxy keeps from
  an extension. **So the token of an extension granted `commands` is an administrator's credential**: it can
  take over a command of the servers, read what is typed after it, and say anything to any player, in the
  chat, as the proxy. Keep its file readable by the proxy and your program only, and grant `commands` only
  to a program you trust that much.

## 8. Giving orders to the proxy

Your program can also ask the proxy for things, on the same connection. The first order is the one that
matters for section 6: when your program changes someone's rights, the proxy may still hold the answers it
kept for them (`cache_seconds`), and it can be told to forget:

```rust
let mut extension = Extension::new("ranks", "1.0.0", token);
let proxy = extension.proxy();            // a handle: clone it, use it from any thread
extension = extension.on_command(decide);

// later, when Alex is taken off the staff list:
loop {
    match proxy.invalidate_player("0a2ea4ba-bced-39d2-80e1-1ad51a0e925c") {
        Ok(()) => break,
        Err(e) if e.retry_later() => std::thread::sleep(std::time::Duration::from_millis(200)),
        Err(e) => { eprintln!("could not tell the proxy: {e}"); break }
    }
}
```

`invalidate_all()` is the same for every player. The order needs the capability `decide.command` (your program
has asked for it as soon as it has `on_command`) and the operator's grant of it. An order the proxy does not
serve to you is answered `-32601`, and `e.is_not_granted()` says so. The proxy limits the orders a program
may give: 20 a second with bursts of 50, and 8 at a time (`[extension.orders]` in the proxy's configuration).

- **A refused order did not happen.** `retry_later()` is true when the proxy's budget or its places were
  spent, or too many of your own orders wait: ask again. **Do ask again for a rights change**, since the right
  you took back stays kept for the `cache_seconds` you gave until the proxy has been told.
- **An order that timed out or whose connection ended may or may not have been done.** Asking again is
  harmless for this order.
- An order waits for its answer on the thread that gave it (5 seconds at most, `Extension::order_timeout`), so
  a handler can give one. At most 64 wait at once.
- With no proxy connected, an order ends at once with `NotConnected`.

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
