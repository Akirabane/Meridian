# Configuration reference

The proxy reads one TOML file, `meridian.toml` by default or the path given on the
command line. [meridian.example.toml](../meridian.example.toml) shows every setting
with comments; this page lists each one with its default, what it does and what is
checked. The checks live in the `meridian-config` crate.

`meridian --check <file>` reads and checks a file, then exits: 0 when it is valid, 1
with a message otherwise. The proxy refuses to start with an invalid file, and the
console's `reload` keeps the configuration in use when the new one is invalid.

## General rules

- Keys are written in kebab-case (`max-players`). A key the proxy does not know is
  refused with its name, and so is the Rust spelling (`max_players`).
- A value of the wrong type is refused with the line it is on.
- Top-level keys come before the first table (`[[backends]]`, `[auth]` and so on), as
  TOML requires.
- The checks stop at the first problem and name the setting at fault.
- Every setting can be left out, except `[[backends]]` and `forwarding-secret`.

## Top-level settings

| Key | Default | What it does | Checks |
| --- | --- | --- | --- |
| `listen` | `"0.0.0.0:25565"` | The address players connect to | An IP address with a port, such as `"0.0.0.0:25565"` or `"[::]:25565"`. A reload cannot change it: it needs a restart |
| `motd` | `"A Meridian proxy"` | The line shown in the server list | None |
| `max-players` | `20` | The most players the proxy takes at once, also shown in the server list. `0` means no limit, and the list then shows one more than the players online | At most 2147483647 |
| `status-sample` | `12` | How many player names the server list shows, the longest online first | At most 100 |
| `on-duplicate` | `"refuse"` | What a second login of a connected player does: `"refuse"` it ("You are already connected to this proxy"), or `"replace"` the session already open | `"refuse"` or `"replace"` |
| `connections-per-minute` | `20` | Connections one address may open in a minute, server list pings included. Further ones are closed before any packet is read. `0` means no limit | None |
| `auth-mode` | `"online"` | How players log in: `"online"`, `"offline"` or `"hybrid"` (see below) | One of the three. Must agree with `online-mode` if both are written |
| `online-mode` | (follows `auth-mode`) | The older form of `auth-mode`: `true` means online, `false` offline | When `auth-mode` is also written, `true` goes only with `"online"` and `false` only with `"offline"`; neither goes with `"hybrid"` |
| `compression-threshold` | `256` | Packets of at least this many bytes are compressed, on both sides of the proxy. A negative value turns compression off | None |
| `connect-timeout-ms` | `5000` | Milliseconds allowed to reach a server | More than 0 |
| `backend-retry-seconds` | `10` | Seconds a server that just failed to take a player is passed over: the next players try it last. `0` never passes a server over | None |
| `forwarding-secret` | none | The secret shared with the servers, which they use to check that a login comes from the proxy. It must be the same as each server's `forwarding.secret`. It never appears in logs or in debug output | Required: may not be empty |
| `default-backend` | the first server listed | The server players are sent to when nothing else chooses one | A configured server |
| `try` | `[]` | Servers tried, in order, after the first choice when it cannot be reached | Configured servers, each once, not the default server |
| `sidecar-timeout-ms` | `5000` | Milliseconds allowed for a sidecar to answer a request or a state reading | More than 0 |
| `switch-carry-position` | `false` | Whether a switch carries the player's position and look to the other server, where their client stands at that moment. Without it they arrive where that server last had them, or at its spawn. Both servers must put the player in the same dimension. It needs servers that take the place in the import request (Horizon from fork commit 46bfe5de0): older ones put the player at the place of the export | None |
| `switch-freeze-limit-ms` | `8000` | Milliseconds a player may stay frozen on the old server during a switch before the switch is given up and the player goes on there. It must be below every server's `[transfer] max_freeze_seconds` (30 s in Horizon by default): the server lets the freeze go by itself after that, and the proxy gives up first only if its limit is lower. The proxy warns at start when the limit is not below 30 s, and each switch also stops before the freeze the old server says is left runs out | More than 0 |
| `switch-commit-margin-ms` | `2000` | Milliseconds kept in hand against the freeze the old server says is left: the new server places the player only while the freeze left outlasts this and two requests as slow as the slowest of the switch so far (the confirm and the release). Otherwise the switch is given up and the player goes on where they were | None |

### Login modes

- `"online"`: every player is checked with the session service, and is *verified*.
- `"offline"`: nobody is checked, and every player is *unverified*. For a closed network
  only.
- `"hybrid"`: a name that belongs to an account must be checked like in the online
  mode; a name that belongs to no account is let in unverified. The names are looked
  up as `[auth]` says. A hybrid configuration needs a `[[route]]` for each kind of
  player, so the two kinds never share a server.

## `[[backends]]`

The servers behind the proxy. At least one is required.

| Key | What it is | Checks |
| --- | --- | --- |
| `name` | The name used in the rest of the file, in commands and in logs | Not empty; unique |
| `address` | Where the server listens, as `host:port` | A host and a port from 1 to 65535, with no space. The host is a name or an IPv4 address, or an IPv6 address in brackets (`"[::1]:25567"`). It is not looked up when the file is read |
| `sidecar` | Where the server's sidecar listens, as `host:port` (the `meridian-sidecar` that runs next to the server). A server without one cannot be switched to or from, and its state is not read | A valid `host:port`; each sidecar address once; needs `sidecar-secret-file` |
| `sidecar-secret-file` | The file that holds the secret the proxy shares with that sidecar. A relative path is taken from the folder of this configuration file. The secret is never written in the configuration | Readable, at least 16 bytes (a final line break is ignored); each server its own file; needs `sidecar` |
| `role` | What the server is for in load balancing: `"lobby"`, `"sublobby"` or `"game"`. Without one, the server is not balanced. Only `"lobby"` is used so far, by `[balancing] enabled` ([LOAD-BALANCING.md](LOAD-BALANCING.md)) | One of the three. Needs `soft-cap` |
| `soft-cap` | The players the server is filled to before the next server of its role is opened | At least 1; only with a `role` |
| `hard-cap` | The most players the balancer ever places on it. Defaults to `soft-cap` | At least `soft-cap`; only with a `role` |

Once any server has a `role`, one of them must be a `"lobby"`.

## `[[forced-host]]`

A host name that sends the players who type it to one server, in place of the default
server. Empty by default.

| Key | What it is | Checks |
| --- | --- | --- |
| `host` | The host name the player typed in their server list. Case does not matter; it is kept in lower case | Not empty; no port, no path, no space and no final dot; each host once |
| `backend` | The server it leads to | A configured server |

## `[[group]]` and `[[route]]`

Groups of servers for one kind of player, and the group each kind is sent to. Both are
empty by default; then every player uses the default server and `try`.

`[[group]]`:

| Key | What it is | Checks |
| --- | --- | --- |
| `name` | The group's name, used by routes | Not empty; unique |
| `auth` | The kind of player it is for: `"verified"` or `"unverified"` | One of the two |
| `backends` | Its servers, in the order they are tried | Not empty; configured servers, each once |

A server may be in several groups of the same kind, but not in groups of both kinds:
the two kinds get different unique ids for one name, and the server would keep two
files for them.

`[[route]]`:

| Key | What it is | Checks |
| --- | --- | --- |
| `auth` | The kind of player it is for | `"verified"` or `"unverified"`; one route per kind |
| `group` | The group they are sent to | A configured group of the same kind |

Once routes are written, each kind of player the login mode lets in needs one: online
needs a verified route, offline an unverified one, and hybrid both (hybrid needs both
even when no route is written). A server in a group for the other kind of player is
never tried for a player, whatever chose it.

## `[auth]`

How the hybrid mode looks names up. The other modes ignore it.

| Key | Default | What it does | Checks |
| --- | --- | --- | --- |
| `lookup-cache-seconds` | `3600` | Seconds a name found to be an account's is remembered. `0` remembers nothing | None |
| `negative-cache-seconds` | `300` | Seconds a name found to be no account's is remembered | Not more than `lookup-cache-seconds` |
| `on-lookup-failure` | `"refuse"` | What happens when the account service does not answer: `"refuse"` the login, or let the player in `"unverified"` | One of the two |
| `lookups-per-minute` | `10` | Lookups one address may cause in a minute; names already remembered do not count. An address over the limit is refused until the minute is over, whatever `on-lookup-failure` says | At least 1 |

## `[balancing]`

How servers that have a `role` are filled. With `enabled` left at `false`, the
proxy chooses a server exactly as it did before there were roles, whatever the roles
say. Of the other settings, `margin`, `state-max-age-seconds` and
`probe-interval-seconds` and `tie-break` are used.
[LOAD-BALANCING.md](LOAD-BALANCING.md) says what each setting is for and what is
still missing.

| Key | Default | What it does | Checks |
| --- | --- | --- | --- |
| `enabled` | `false` | Whether logins are placed by the balancer among the servers of role `"lobby"` that the player's route allows. Off, the proxy chooses a server as it always did | Needs at least one server with a `role` |
| `margin` | `2` | How far below its `soft-cap` a server that reached it must fall before it is filled again. `0` turns this off. A margin larger than the soft cap means the server is filled again only when empty | None |
| `state-max-age-seconds` | `15` | Seconds after which what a server last reported is no longer believed; it is then not offered to new players. A server that never reported is treated as accepting | More than 0 |
| `tie-break` | `"ping"` | How a tie between equally full servers is broken: `"ping"` (the one the proxy measured as quickest to connect to) or `"order"` (the one listed first). With `"order"` the proxy does not measure | `"ping"` or `"order"` |
| `probe-interval-seconds` | `10` | Seconds between two readings of the state of each server with a role and a sidecar, through its sidecar, and between two measures of the time a connection to each server with a role takes | More than 0 |

## `[restart]`

What the proxy does when a backend with a role is about to restart ([RESTART.md](RESTART.md)).
Whatever these say, such a backend is marked draining as soon as the proxy believes it is
going down, and offered again when it is back. The players stay on it unless `evacuate` is on.
Without the table, nothing moves a player.

| Key | Default | What it does | Checks |
| --- | --- | --- | --- |
| `evacuate` | `false` | Whether the players of a backend that is going down are sent to other servers of its role, one at a time, as `drain <server> --move` sends them. A player with nowhere to go stays and is named in the log. Off, the players stay on the backend | `true` or `false` |
| `silent-rounds` | `3` | How many readings in a row (one every `[balancing] probe-interval-seconds`) its sidecar may leave unanswered before the backend counts as going down. Needs `[balancing] enabled` and a sidecar | More than 0 |
| `grace-seconds` | `60` | Seconds a backend that was told to stop may go on answering before it counts as back: it answers for a while as it shuts down | More than 0 |

A reload applies the three keys to the next reading.

## `[proxy-protocol]`

Whether the proxy reads the [PROXY protocol](https://www.haproxy.org/download/2.9/doc/proxy-protocol.txt)
header, version 1 or 2, that a layer 4 load balancer (HAProxy, nginx `stream`, a host's
balancer) writes at the start of each connection to say who the player really is. The
address it gives is the one the proxy uses for its log, for `connections-per-minute` and
`[auth] lookups-per-minute`, and for the address of the player that it forwards to the
servers. Off by default: the proxy then reads no header, as before.

The header is whatever its sender writes, so anyone who may send it can claim any address.
The proxy therefore reads it **only from the addresses in `trusted`**, never from a player's,
and refuses `optional` and `required` without that list. From any other address, the proxy
does not look for a header at all: a client that sends one gets nothing out of it.

| Key | Default | What it does | Checks |
| --- | --- | --- | --- |
| `mode` | `"off"` | `"off"`: no header is read. `"optional"`: a connection from a trusted address may begin with a header or not, and one from any other address is a player's. `"required"`: a connection from a trusted address must begin with a header, and one from any other address is closed at once, so the proxy takes players only through the balancers | `"off"`, `"optional"` or `"required"` |
| `trusted` | `[]` | The addresses of the load balancers, as IP addresses or ranges (`"10.0.0.5"`, `"192.168.0.0/16"`, `"2001:db8::/32"`). An IPv4 address written as IPv6 is the IPv4 address | Not empty unless `mode` is `"off"`; each entry an address or a range; a range of every address (`/0`) is refused |
| `header-timeout-ms` | `3000` | Milliseconds a trusted connection has to send the whole header. A balancer that is silent for longer is closed | More than 0 |

What the proxy accepts of a header is bounded: a version 1 line of at most 107 bytes, a
version 2 header of 16 bytes and at most 536 more (the extensions are skipped, not read).
A header that is malformed, too long, late or missing where it is required closes the connection
without a reply and logs a warning with the balancer's address. A header that says nothing about
the player (`LOCAL`, `UNKNOWN`, `UNSPEC`, a Unix socket: a balancer's own check) leaves the
address of the connection. The limit `connections-per-minute` is counted on the player's address
for a trusted connection, not on the balancer's. A reload applies the table to the connections
that come after it.

## `[metrics]`

A page of counters in the Prometheus text format, and a health check
([OPERATING.md](OPERATING.md), section 8). Off by default. It is a small HTTP server of
the proxy's own, with two paths: `GET /metrics` and `GET /healthz`. There is no password
and no TLS, so it listens on the loopback unless you say otherwise.

| Key | Default | What it does | Checks |
| --- | --- | --- | --- |
| `enabled` | `false` | Whether the proxy serves the page | `true` or `false` |
| `bind` | `"127.0.0.1:9100"` | The address and port it listens on. An address other machines can reach is logged as a warning at start | A `host:port`; with `enabled`, a port above 0 that is not the one in `listen` |

The page names no player: its labels are the names of the servers in this file and fixed
words. A reload does not change it: `enabled` and `bind` are read when the proxy starts.

## `[commands]`

The commands players type in game that the proxy answers itself.

| Key | Default | What it does | Checks |
| --- | --- | --- | --- |
| `enabled` | `["server", "hub"]` | The commands the proxy answers. `"server"` is `/server`; `"hub"` is `/hub` and `/lobby`; `"play"` is `/play`, which takes the player to the server of role `game` the balancer finds (it needs `[balancing] enabled` and a server with that role, and is off unless listed). Any other command goes to the server | `"server"`, `"hub"` or `"play"`, each once; may be empty |
| `hub-backend` | the default server | The server `/hub` and `/lobby` lead to. With `[balancing] enabled` and a server of role `"lobby"`, leaving it out lets the balancer choose the lobby; writing it keeps that server | A configured server |
| `server-list` | `true` | Whether `/server` may list the servers | None |

## What a reload changes

`reload` in the console reads the file again. When it is valid, it applies to the
logins that start after it: players already connected keep their server and their
session. A server removed from the file keeps its players until they leave. `listen`
is the exception: the proxy keeps listening where it started until a restart.
