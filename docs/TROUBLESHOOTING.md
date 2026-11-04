# Troubleshooting

Each entry is a message that Meridian prints or a player sees, what it means, and what to do.
The messages are copied from the code (`crates/meridian-proxy`, `meridian-net`,
`meridian-handoff`, `meridian-sidecar`, `meridian-config`) or from the runs in
[RUNNING.md](RUNNING.md) and [TESTING.md](TESTING.md). A word in angle brackets, such as
`<name>`, stands for the real value. [OPERATING.md](OPERATING.md) is the guide this page
belongs to.

The proxy writes its log to standard error as `2026-11-04T17:04:05.123Z WARN  ...`. Set
`MERIDIAN_LOG=debug` to see more (each connection, each switch step). The sidecar prints to
standard error too, with the prefix `meridian-sidecar:`.

## Starting the proxy

| Message | Cause | Action |
| --- | --- | --- |
| `unknown option <x>` | A mistyped option. The proxy has `--check`, `--help` and `--version` only | Run `meridian --help` |
| `only one configuration file can be given, not also <x>` | Two paths on the command line | Give one |
| `<path>: <error>` at start, or `meridian --check` exiting with 1 | The configuration file cannot be read or is invalid. The message names the setting | Fix the file; `meridian --check <file>` repeats the check without starting |
| `backends have roles but none has role = "lobby": players need a lobby to arrive in` | A backend has a `role`, none is `"lobby"` | Give one backend `role = "lobby"` |
| `[balancing] enabled = true, but no backend has a role: ...` | Balancing on, no backend has a role | Give roles and `soft-cap`s, or turn balancing off |
| `backend "<name>" has a sidecar but no sidecar-secret-file: ...` (or the reverse) | `sidecar` and `sidecar-secret-file` go together | Write both, or neither |
| `backends "<a>" and "<b>" both have the sidecar <address>: ...` / `both use the secret file <path>: ...` | Two backends share a sidecar address or a secret | Each backend needs its own sidecar and its own secret file |
| `the sidecar secret of backend "<name>": ...` | The secret file cannot be read, or holds fewer than 16 bytes | Write a secret of 16 bytes or more; the path is relative to the configuration file's folder |
| `cannot listen on <address>: <error>` | The address is taken, or not one of this machine's, or needs a privilege | Free the port or change `listen`. A reload cannot change `listen` |
| `cannot start the console: <error>` | The thread that reads standard input did not start | The proxy runs without a console, stops on Ctrl+C only |
| `switch-freeze-limit-ms is <n> ms, but a Horizon server lets a frozen player go by itself after its max_freeze_seconds (30 s by default): ...` | The proxy's freeze limit is not below Horizon's default | Lower `switch-freeze-limit-ms`, or raise `max_freeze_seconds` in `[transfer]` on every server that has a sidecar |

## A player cannot join

What the player sees, then what the log says.

| The player sees | The log says | Cause | Action |
| --- | --- | --- | --- |
| `Cannot reach the server "<name>" right now. Try again in a moment.` (or `Cannot reach the servers "<a>", "<b>" ...`) | `WARN <peer>: <player> (<uuid>) cannot join backend "<name>" at <address>: <error>` | No candidate could be reached. Read `<error>`, below | See the next table |
| `There is no server to join right now.` | `WARN ...: no backend to send them to: <error>` | The routes give this kind of player no server (for example an unverified player and only a verified group) | Check `[[route]]` and `[[group]]`: [CONFIG.md](CONFIG.md) |
| `The server is full` | `INFO ... refused: the proxy has its <n> players` | `max-players` reached | Raise `max-players`, or accept the refusal |
| `The server is full` | `INFO ... refused: every lobby is at its hard cap` | With balancing on, every lobby is at its `hard-cap` | Add a lobby or raise caps |
| `You are already connected to this proxy` | `INFO ... refused: already connected` | `on-duplicate = "refuse"` and the same player is online | Wait, or set `on-duplicate = "replace"` |
| `multiplayer.disconnect.transfers_disabled` | none | The client used a server transfer to the proxy; the proxy takes players only by a normal login | Players should connect with the address, not a transfer |
| `multiplayer.disconnect.outdated_client` / `incompatible` | none | The client's protocol is not the one of the game version the proxy speaks | Use the matching client version |
| `multiplayer.disconnect.authservers_down` | none | Online mode: the session service did not answer | Wait; check the proxy's network. `[auth] on-lookup-failure` is for hybrid mode only |
| The connection closes right after the server list | `WARN <peer>: <error>; closing the connection` | The client sent something the proxy does not accept | Read `<error>`; with `MERIDIAN_LOG=debug`, look at the lines before it |
| Nothing: the connection is closed without a message | `DEBUG <peer>: over <n> connections this minute; closed` | `connections-per-minute` (20 by default) reached for that address. Server list pings count | Raise the limit, or `0` for none. Many players behind one NAT address can reach it |

Reasons after `cannot join backend`:

| `<error>` | Cause | Action |
| --- | --- | --- |
| A system error such as `Connection refused` or a timeout | The server is off, its port is closed or the address in `address` is wrong. The timeout is `connect-timeout-ms` | Start the server; check `address` and the firewall. A server that failed is tried last for `backend-retry-seconds` |
| `the backend refused the login: ...` followed by Horizon's reason. With a wrong secret Horizon says `the forwarded data is not signed with this server's secret` | `forwarding-secret` differs from the server's `forwarding.secret` | Make them the same text, with no newline at the end of the file |
| `the backend asks players to authenticate: it must run with online mode off and the player information forwarding on` | The server has `online_mode = true`, or no `forwarding.secret` | Set `online_mode = false` and add `forwarding.secret`. The server's log must say `Velocity player information forwarding is on` |
| `the backend sent login packet <n>, which is not understood` | The server is not a Horizon server of a version the proxy speaks | Check the server and the pinned protocol commit ([BUILD.md](BUILD.md)) |

A player who joins a server directly instead of the proxy is refused by Horizon with `This
server is behind a proxy: join through the proxy.`: that is the forwarding working.

## During a session

| Message | Cause | Action |
| --- | --- | --- |
| `INFO ... joined backend "<name>"` | A login worked | None |
| `INFO ... left backend "<name>" after <s> s: <n> packets, <n> bytes to the player; ...` | A session ended normally | None |
| `WARN ... on backend "<name>": the session broke after <s> s: <error>` | The connection to the player or to the server broke | The server stopped or the network dropped. Look at the server's log at that time. The player is not moved to another server |
| `WARN <peer>: <player>: cannot set up the session: <error>` | The proxy could not set up the relay after the login | Rare; report it with the debug log |
| `WARN <n> connections are still open; they close when the proxy exits` | At stop, some sessions did not end in five seconds | None; they close as the process ends |

## Switching a player

A failed switch prints one line on the console and in the log, and tells the player `Could not
move you to <server>. <reason>`:

```
<player> could not move from <from> to <target>: <step>: <reason> (<where the player is>)
```

The step is `check`, `prepare`, `freeze`, `hand over` or `commit`. The player stays where they
were unless the end of the line says otherwise ([OPERATING.md](OPERATING.md) section 10).

| Reason | Cause | Action |
| --- | --- | --- |
| `check: there is no server called <x>` | A typo in the target | `servers` lists the names |
| `<player> is already on <target>` | The player is on the target | None |
| `check: a switch of this player is already under way` | A second switch started before the first ended | Wait for the first |
| `check: <server> has no sidecar in the configuration, so no player can be switched to or from it` | The source or the target has no `sidecar` setting | Add `sidecar` and `sidecar-secret-file` to that backend, then `reload` |
| `check: the sidecar of <server>: <error>` | The proxy cannot reach or authenticate to the sidecar | See "The sidecar" below |
| `check: the player is not in the play phase` / `the player has not joined yet` | The player is still logging in, or is in a configuration screen | Try again |
| `prepare: <target> would not expect the player: <error>` | The target refused `player-expect`. With Horizon's `{"error": "transfers are off on this server"}`, its `[transfer] enabled` is false; with `unknown request`, it is a Horizon build with no transfer module | Turn on `[transfer] enabled = true` on every server, or use a Horizon build that has the module |
| `prepare: the player could not log in on <target>: <error>` | The proxy could not log the held player in on the target (the forwarding secret, or the server is full or off) | Same as "cannot join backend" above |
| `prepare: the target did not send the player's world in time` | The target is slow, or overloaded | Try again; check the target's load. `MERIDIAN_LOG=debug` shows the steps |
| `prepare: <target> puts the player in <x>, and they are in <y>: only a new login changes the dimension` | The two servers put the player in different dimensions | Switch between servers that put the player in the same dimension; the proxy cannot change a dimension without a new login |
| `prepare: <target> configures clients differently (<kind>): the player would need a new configuration phase` | The two servers differ in what the client holds at login (data packs, registries, tags) | The servers must be configured alike; this is not fixable by the proxy |
| `prepare: <target> does not enforce signed chat as the player's first server does` | One server enforces secure chat, the other not | Neither should enforce it ([README](../README.md)) |
| `freeze: the player's server would not give their record: <error>` | The source refused `player-export` (the player is riding, asleep or dead), or it did not answer | Try again when the player is on foot. `unreached` in the error means no answer: see below |
| `freeze: the player's server gave no hand-over id with their record: it does not take part in transfers as this proxy asks` | The source is a Horizon build older than the transfer contract | Update the servers |
| `freeze: the player's freeze has <n> ms left on the old server, and the rest of the transfer needs about <n> ms` | Not enough freeze left (slow servers or sidecars) | Raise `max_freeze_seconds` on the servers, then `switch-freeze-limit-ms` below it; lower `switch-commit-margin-ms` only if you understand it ([CONFIG.md](CONFIG.md)) |
| `hand over: ...` with a chunks or packets message | The target's world could not be shown to the client in time | Try again; report it with the debug log |
| `commit: the source would not let the player go: <error>`, then an `ERROR ALERT ...` line | The target placed the player and the source refused to release them | See "An ALERT line" below |

An export that gets no answer is asked again twice, on a new connection, which is safe (the
server answers the same record). If every try is lost, the switch is given up; the source may
have frozen the player, who then cannot move until `max_freeze_seconds` passes.

### An ALERT line

```
ERROR ALERT <player>: transfer <id> from <a> to <b>: <a> would not let the player go (<reason>). <player> is on <b>, which placed them and saved their files; <a> keeps its own copy and saves it, so the two servers' files for <player> now differ: compare them and keep the one that is right. Both servers' logs name the transfer <id>
```

The player is on `<b>`. Find `<id>` in both servers' logs (each `player-*` line has `hand-over
<id>`), then compare `players/data/<uuid>.dat`, advancements and stats on the two servers. The
file on `<b>` is the right one unless the player died on `<a>`, or a command changed them there
in the freeze's last seconds. Nothing is merged for you.

## The sidecar

The proxy's `check: the sidecar of <server>: <error>` and `cannot read the state of backend
"<name>": <why>` carry one of these:

| `<why>` or sidecar message | Cause | Action |
| --- | --- | --- |
| `cannot reach it at <address>: <error>` | Nothing listens at `sidecar = "<address>"`, or a firewall blocks it | Start `meridian-sidecar`; check `--listen` and the address in the proxy's file |
| `cannot read <path>: <error>` / `<path> holds <n> bytes; a sidecar secret has at least 16` | The proxy cannot read the sidecar's secret file | Fix the path (relative to the proxy's configuration file) and the file |
| `the signature does not match the shared secret` (or `sequence number <n> where <m> was expected`) | The proxy and the sidecar hold different secrets, or a line was changed or replayed on the way | Give both the same secret. A final line break is ignored, any other difference is not |
| `the sidecar refused: unknown request` | The Horizon server does not know the request. For `status` this is normal today (see below). For a `player-*` request it probably means a build with no transfer module | Use a Horizon build with the transfer module |
| `{"ok": false, "error": "...", "unreached": true}` | The sidecar could not get the server's answer: no answer in time, the control endpoint closed, or its file unreadable while the server restarts | The server is down, restarting or hung. `--endpoint-timeout-ms` (default 5000) bounds the wait |
| `meridian-sidecar: --endpoint-file is required` | The option is missing | Give `--endpoint-file <server folder>/.horizon/engine.json` |
| `meridian-sidecar: no secret: give --secret-file or set MERIDIAN_SIDECAR_SECRET` | No secret | Give one |
| `meridian-sidecar: the secret is <n> bytes; use at least 16` | The secret is too short | Use a longer one |
| `meridian-sidecar: cannot read <path>: <error>` | The secret file cannot be read | Check the path and the account's rights |
| `meridian-sidecar: <address> is not a local address: the sidecar listens on 127.0.0.1 unless --allow-remote is given` | `--listen` is not a loopback address | Keep it on `127.0.0.1` and reach it over a tunnel, a VPN or a private network, or read [OPERATING.md](OPERATING.md) section 4 |
| `meridian-sidecar: <address> is reachable from other machines, and the sidecar has no TLS yet: ...` | `--allow-remote` without `--insecure-remote` | The sidecar has no TLS. Do not open it to a network you do not control |
| `meridian-sidecar: WARNING: <peer> connected without TLS: player records cross the network in clear` | `--insecure-remote` is on and a machine other than this one connected | Expected with that flag; move to a tunnel |
| `meridian-sidecar: cannot listen on <address>: <error>` | The port is taken | Another sidecar, or change `--listen` |

## Balancing and the state of servers

| Message | Cause | Action |
| --- | --- | --- |
| `WARN backend "<name>" does not answer the state request: its state is unknown, so it is treated as accepting players` | The Horizon server answered `unknown request` to the state read. Horizon has not defined that command yet | None. The server is alive and is balanced on the proxy's own count. Logged once |
| `WARN cannot read the state of backend "<name>": <why>` | The sidecar is silent or refuses (see "The sidecar"). Logged once per failure run | Fix the sidecar. After `silent-rounds` readings the server counts as going down |
| `INFO backend "<name>" reports its state again` | It answers again | None |
| `servers` says `not offered (no report for <n> s)` | No answer for `state-max-age-seconds` | The sidecar or the server is down. The server takes no new player until it answers |
| `servers` says `draining (drained from the console)` | An operator typed `drain` | `undrain <server>` when ready |
| `servers` says `draining (it does not accept transfers)` | The server reports `accepts_transfers = false` | Turn it back on in the server, or it stays closed to new players |
| `servers` says `closed (soft cap reached)` | At `soft-cap`; it opens again below `soft-cap - margin` | None |
| `<server> has no role, so there is nothing to drain; it takes players only where a route, a forced host or a command sends them` | `drain` on a server with no `role` | Give it a `role` and `soft-cap`, or just stop it |

## Restarts and evacuation

| Message | Cause | Action |
| --- | --- | --- |
| `WARN backend "<name>" is going down (<cause>): it takes no new player` | The proxy believes the server is stopping. `<cause>` is `it announced its stop`, `the panel is restarting it` or `it stopped answering` | None; the server is marked draining |
| `INFO backend "<name>" is going down (<cause>): its players stay on it ([restart] evacuate is off)` | The evacuation is off, the default | Turn on `[restart] evacuate = true` if you want them moved |
| `INFO backend "<name>" is going down: nobody is on it` | The evacuation had nobody to move | None |
| `INFO backend "<name>" is going down: <n> moved, <m> stayed` | The evacuation ran | Look at the `stays on` lines for the rest |
| `WARN <player> stays on <server>: <why>` | No other server of the role has room, or the move failed | Add capacity of the same role; the player is disconnected when the server stops |
| `WARN cannot start the evacuation: <error>` | The proxy could not start the thread | Rare; the server is still drained |
| `INFO backend "<name>" is back` | The server answers again (or `server/started`) | The server is offered again, unless you drained it yourself |
| `servers` says `draining (restarting: <cause>)` | A restart is under way | Wait for it to end |
| A server stays `draining (restarting: ...)` long after it came back | It never stopped answering for a reading, and `grace-seconds` has not passed | Wait `grace-seconds` (60), or `undrain <server>` |

## The console

| Message | Cause | Action |
| --- | --- | --- |
| `unknown command <x>; type help for the list` | A mistyped command | `help` |
| `usage: <command> ...` | Wrong arguments | Read the usage line |
| `<player> is not online` | `kick`, `switch` or `send` with a name that is not on the proxy | `list` |
| `WARN the console cannot read its input: <error>` | Standard input broke | Restart the proxy under a terminal or a multiplexer |
| Commands typed in a service do nothing | The proxy has no terminal, so no console | Run it in `tmux` or `screen`, or under the panel ([OPERATING.md](OPERATING.md) section 5.4) |
| `not reloaded, <path> is unchanged in use: <error>` | The new file does not pass the checks | The previous configuration stays in force; fix the file and `reload` |
| `reloaded: nothing changed` | The file is the same as the one in force | None |

## The panel

| Message | Cause | Action |
| --- | --- | --- |
| `the Minecraft EULA is not accepted` | A server will not start until you accept it | Read the EULA and run `meridian-panel accept-eula --dir <bench> --i-accept-the-minecraft-eula` |
| `no prepared game files` in a server's console | `--resources` does not point at the prepared game files | Prepare the files (Horizon's "Host a server"), pass `--resources` to `init` |
| `cannot listen on 127.0.0.1:8140` | Another panel is running | Close it, or change `panel-port` |
| `another engine still holds the world: pid <n>` | The engine of an earlier run is still alive | End that process, then start the server again |
| `[drain-before-stop] is enabled: name the proxy to tell with proxy = "...", the bench has not exactly one proxy` | The bench has several proxies, or none | Write `proxy = "<process name>"` |
| The proxy's console shows `[stderr]` lines | That is where the proxy writes its log; it is not an error | None |
| The proxy logs `Connection closed by the server` each time the panel checks it | That line is the panel's check | None |
