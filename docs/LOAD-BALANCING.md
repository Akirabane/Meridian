# Load balancing

This is a design note. It says what the proxy needs to place players across several
servers of one kind, what Meridian already has for it, and what is missing. What
exists so far: the settings (`meridian-config`), the placement as pure functions
(`balance.rs`), and the login using them for the servers of role `lobby` when
`[balancing] enabled = true` (`placing.rs`). With `enabled` left at `false`, which is
the default, nothing changes: the proxy chooses a server as it always did.

Placing players across servers of one role is in scope for Meridian
([DESIGN.md](DESIGN.md) section 14 lists it). Matchmaking rules, meaning who plays
with whom, are not part of it.

## 1. What is wanted

1. **Roles.** Each server is a `lobby`, a `sublobby` (a waiting room where players
   are grouped before a game, as in matchmaking) or a `game` server.
2. **State.** For each server the proxy knows how many players it has, how many it
   can take, and whether it accepts transfers.
3. **The player.** Whether they are verified (an account checked by the session
   service) or unverified, and how quickly the proxy reaches them.
4. **Placement.** Fill one server up to an honest number, then open the next. Not one
   player per server. A server that stops accepting transfers is drained. Ping breaks
   ties.

## 2. What exists, and what is missing

| Need | Today | Missing |
| --- | --- | --- |
| Roles | `role`, `soft-cap` and `hard-cap` are read and checked. At login, role `lobby` is used | The use of `sublobby` and `game`: moving players between roles |
| Entry point | `default-backend`, `try`, and `hub-backend` for `/hub`: one server each. With balancing on, a login is placed among the lobbies its route allows | `/hub` and `/server` do not use the balancer yet |
| Order of candidates | `route::candidates` gives the servers of the player's group in the listed order, and a login goes to the first that answers. Filling a server first is therefore what happens already, with no limit at all | A limit: the first server takes every player until it fails |
| Player count per server | The proxy knows the backend of each session (`Sessions::players`). It is exact because backends are private and the proxy is the only way in (DESIGN.md section 12, decision 9) | `Sessions::counts` gives the count. A place is reserved with the count, under one lock, from the choice of a server until the session is open, so logins at the same time do not pass a cap. Players who joined a server some other way are not seen |
| Capacity | `max-players` limits the whole proxy, not a server | Read and checked by this step (`soft-cap`, `hard-cap`) |
| Does a server accept transfers | The proxy reads `accepts_transfers`, `players` and `capacity` through the sidecar (section 3). A failed login also marks a server "passed over" for `backend-retry-seconds` (`health.rs`) | The answer on the Horizon side: its command and its names are not fixed |
| Premium or offline | Done. Players are verified or unverified, decided at login (DESIGN.md section 15), and groups and routes keep the two kinds apart | Nothing for placement: it chooses among the servers the route already allows |
| Ping | The console's `servers` command measures how long a TCP connection to a backend takes. Nothing measures a player. The relay passes keep-alives through as they are (`relay.rs`) | A measure kept by the proxy, see section 5 |
| Drain | Nothing. A server removed from the file keeps its players until they leave (CONFIG.md, "What a reload changes") | Stop offering a server and let it empty |
| Moving players between roles | `/server` and `/hub` switch a player through the sidecars (DESIGN.md section 5) | Moving a group from a sublobby to a game, and who decides |

## 3. Reading the state of a server

The sidecar protocol has seven requests: `Expect`, `Export`, `Import`, `Confirm`,
`Release`, `Drop`, and `Status` (`crates/meridian-sidecar/src/request.rs`). The first
six move a player. `Status` only reads, names no player, and goes through the same
signed exchange as the others (`wire.rs`).

The direction chosen: the sidecar already talks to its server, so it asks the server
for its state through the server's control endpoint and passes the answer up to the
proxy, which asks the sidecar every `probe-interval-seconds`. No new port is opened.

What exists, and what does not:

- **The sidecar side is done.** `Request::Status` becomes one line on the endpoint,
  `status` (`STATUS_LINE`), and the answer is passed up as the backend gave it, like
  every other answer. `SidecarClient::status` reads it as a `ServerStatus`.
- **The Horizon side is not.** The Horizon server has not fixed the command that
  reports its state, nor the names in its answer. `status` is a placeholder and
  `STATUS_LINE` is the one place to change. Until a server answers it, the proxy
  hears nothing from it and treats it as accepting players, which is what it does
  without balancing.
- **What the proxy reads** from the answer is three optional fields: `players` and
  `capacity` (whole numbers) and `accepts_transfers` (true or false). A field that is
  left out is unknown, not zero or false. Any other field, such as an activity, is
  ignored. A field of the wrong type makes the whole answer an error. Changing these
  names is a change in `ServerStatus::from_value`.
- **The heartbeat is the answer itself.** The proxy keeps the time of the last answer
  of each backend (`status.rs`). It is believed for `state-max-age-seconds`.

How the proxy uses it, for the backends that have a role and a sidecar, while
`[balancing] enabled` is on:

- A backend that has **never answered** is treated as accepting, with its own count of
  players.
- A backend that answered but **not within `state-max-age-seconds`** is not believed
  and is not offered to new players, until it answers again. Its players stay.
- A fresh `accepts_transfers = false` means the same: it is not offered, not even to
  a forced host.
- A fresh `players` larger than what the proxy counts on the backend replaces the
  count, since the backend may have players the proxy did not send. A smaller one
  changes nothing.
- A fresh `capacity` below a cap written in the file lowers that cap, and the soft cap
  with it. A larger one raises nothing: the caps of the file are the operator's choice.

A failed poll keeps the last answer, which then grows old. The first failure of a
backend is logged once, and so is its recovery.

## 4. Placement

The rule applies to the servers of one role that the player's route already allows.
Roles do not replace groups and routes: a verified player never reaches a server of
an unverified group, whatever the balancer would prefer (DESIGN.md section 15.3).

1. **Candidates.** Leave out a server that does not accept transfers, one whose report
   is too old, one that failed a login within `backend-retry-seconds`, and one at its
   `hard-cap`.
2. **Open and closed.** A server is *open* below its `soft-cap` and *closed* at it. A
   closed server opens again only when it falls to `soft-cap - margin` or below, so a
   server at its cap does not flip with each player who leaves. Between the two levels
   a server keeps the state it had, so this state is kept per server.
3. **Pick.** Among the open servers, take the fullest. This is what fills one server
   before opening the next, rather than spreading players thinly. If two have the same
   count, `tie-break` decides: the server that answers quickest (`"ping"`), or the one
   listed first (`"order"`). A server with no measure yet is ranked as listed.
4. **Overflow.** If no server is open, take a closed one that is below its
   `hard-cap`, the emptiest first. If there is none, the player is told the server is
   full, as `max-players` does today.
5. **Drain.** A server that stops accepting transfers is dropped from step 1. Its
   players stay until they leave, and it is not emptied by force. A server that is
   draining and empty can be stopped.

A forced host and `/server <name>` name a server on purpose. They may go past the
`soft-cap` but never past the `hard-cap`, and a draining server refuses them too.
Because `hard-cap` defaults to `soft-cap`, a server with no `hard-cap` written takes
no overflow from them either.

`/hub` and `/lobby` do not name a server. With roles, they take the best open server
of role `lobby` by the rule above, and a `sublobby` only when no lobby can take the
player. Within the player's route, as always. A written `hub-backend` still wins, as
an explicit choice, and then follows the rule for explicit names. Without roles
nothing changes.

What the rule does not do: it does not group players for a match, it does not decide
when a game server starts or stops, and it does not move players that are already
placed to even out the load. Those are later steps.

## 5. The ping

The ping a player has to the proxy is the same whichever server is chosen, so it
cannot tell two servers apart. Two things are worth measuring instead:

- **Proxy to server**, the time a connection to the server takes. The console already
  does this once on request (`probe` in `state.rs`). Measured every
  `probe-interval-seconds` it is what `tie-break = "ping"` compares. This matters when
  the servers are not all in one place.
- **Player to proxy**, kept for later. It would matter with more than one proxy, or to
  group players with a similar ping in a matchmaking server. It is not measured today:
  the login has no round trip that is not part of the account check, and the relay
  passes keep-alives through untouched. The cheap way is to time the encryption request and its
  answer for a verified player, and to send a keep-alive of the proxy's own after the
  login for any player. This note does not ask for it.

## 6. The settings

```toml
[[backends]]
name = "game-1"
address = "127.0.0.1:25568"
role = "game"        # "lobby", "sublobby" or "game"; no role: not balanced
soft-cap = 12        # filled to this before the next server of the role opens
hard-cap = 16        # the most it ever takes; defaults to soft-cap

[balancing]
margin = 2                      # a closed server opens again at soft-cap - margin
state-max-age-seconds = 15      # an older report is not believed
tie-break = "ping"              # "ping" or "order"
probe-interval-seconds = 10     # how often each server's answer time is measured
```

Checks, all in `meridian-config` (the full table is in [CONFIG.md](CONFIG.md)):

- `soft-cap` and `hard-cap` need a `role`, and a `role` needs a `soft-cap`.
- `soft-cap` is at least 1 and `hard-cap` is not below it.
- Once any server has a role, one of them is a `lobby`.
- `state-max-age-seconds` and `probe-interval-seconds` are more than 0.
- A `margin` larger than a soft cap is not an error: that server opens again only when
  empty.

A server with no role is not balanced. A configuration with no roles is read as it
is today.

Not in the settings on purpose: the number of players per game (that belongs to the
mode, and to the later matchmaking step), and a per-kind cap. A server serves one
kind of player already (section 4).

## 7. Order of work

1. This note and the settings. Done.
2. A count of players per backend and the placement of section 4 as pure functions
   over the counts, the limits and what the caller knows of each server, tested
   without any connection, as `route.rs` is (`balance.rs` in `meridian-proxy`). Done.
3. The login uses it, behind `[balancing] enabled` (off by default), for the servers
   of role `lobby`: a place is reserved while the player is sent, a server that
   does not answer is left for the next, and a forced host may pass the soft cap but
   not the hard cap. Done. Not done: `/hub` and `/server`, and the sublobby and game
   roles.
4. The probe of section 5, kept by the proxy, and the console's `servers` command
   showing the role, the count against the caps, open or closed, and draining.
5. The report of section 3: the sidecar request and the proxy's reading of it are done.
   What is missing is the Horizon server's own command, and the drain it allows once
   a server can say it no longer accepts transfers.
6. Moving players between roles (lobby to sublobby to game). This one needs a decision
   on who starts a move and is not designed here.

## 8. Decided, and still open

Decided:

- Placing players across servers of one role is in scope (DESIGN.md section 14).
- The server's state reaches the proxy through the sidecar (section 3).
- Explicit choices (forced host, `/server <name>`) may pass the soft cap, never the
  hard cap (section 4).
- `/hub` looks for a lobby first, then a sublobby (section 4).

Open:

- The command and the field names of the Horizon server's report, until it defines
  them (section 3). The proxy side reads `players`, `capacity` and
  `accepts_transfers`.
- Whether `default-backend` should follow the roles too, so a new login lands on the
  lobby with room. The rule of section 4 allows it; it is not decided.
- What a player sees when every server of a role is at its hard cap. Today it is "The
  server is full".
- A reload changes the caps for the logins after it, as for every other setting. The
  open or closed state of a server is meant to be kept across a reload.
