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
| `switch-carry-position` | `false` | Whether a switch carries the player's position and look to the other server, where their client stands at that moment. Without it they arrive where that server last had them, or at its spawn. Both servers must put the player in the same dimension | None |

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

## `[commands]`

The commands players type in game that the proxy answers itself.

| Key | Default | What it does | Checks |
| --- | --- | --- | --- |
| `enabled` | `["server", "hub"]` | The commands the proxy answers. `"server"` is `/server`; `"hub"` is `/hub` and `/lobby`. Any other command goes to the server | `"server"` or `"hub"`, each once; may be empty |
| `hub-backend` | the default server | The server `/hub` and `/lobby` lead to | A configured server |
| `server-list` | `true` | Whether `/server` may list the servers | None |

## What a reload changes

`reload` in the console reads the file again. When it is valid, it applies to the
logins that start after it: players already connected keep their server and their
session. A server removed from the file keeps its players until they leave. `listen`
is the exception: the proxy keeps listening where it started until a restart.
