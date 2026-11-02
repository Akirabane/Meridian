# Meridian design

Meridian is a proxy for Horizon servers, written in Rust, with one goal that
shapes everything else: a player moves from one server to another and sees
nothing happen. No loading screen, no black frame, no change in the world, no
reconnect. The player is in the lobby, walks into a portal, and a moment later is
standing in the same place on the map of `game-1`, and later on the map of
`game-2`, without being able to tell.

This document describes how that can work, what it costs, and in which order to
build it. It is a design, not a specification: where a choice is open it says so
and gives a recommendation.

## 1. Scope

In scope:

- A proxy that sits between players and any number of Horizon servers, speaks the
  Minecraft Java protocol on both sides, and is the only thing a player connects to.
- A seamless switch of a player between two backends that run the same map.
- A way for backends to hand a player's state to each other.

Out of scope for now:

- The lobby. It will be a small spawn world served by an ordinary Horizon
  server, and the proxy treats it like any other backend.
- Switching between backends that run different maps, different versions or
  different dimensions. Those can fall back to an ordinary, visible transfer.
- Anything that is not Horizon. The proxy relies on the protocol of the Horizon
  version it is built against and on a few features that only Horizon servers and
  clients will have.

## 2. The idea

A vanilla client builds a new world, and shows its loading screen, whenever the
server sends it a Login (join game) or a Respawn packet, and it drops its
connection state when it is told to transfer or to return to the configuration
phase. Every ordinary proxy switches servers with one of those, which is exactly
why the switch is visible.

Login and Respawn are not the only ones. The proxy also holds back Start
Configuration and Transfer, a Disconnect from the old server, and the game event
that tells the client its world has started loading, which belongs to a Login or a
Respawn. [PACKETS.md](PACKETS.md) lists every packet of the Configuration and Play
states and what the proxy does with each.

Meridian never sends them. From the moment a player finishes logging in until
they leave, the client stays in the Play state, in the same world, with the same
player entity. What changes at a switch is not the connection but the proxy's
source of packets, and the proxy makes that change invisible by sending the
client only the difference between the world it believes it is in and the world
the new server describes.

To do that the proxy keeps, for each player, a *ghost*: a model of everything the
client currently holds. Chunks that are loaded, entities that are visible, the
tab list, boss bars, the scoreboard, effects, the inventory and the open
container, experience, health and food, the time, the weather, the world border,
the difficulty, the spawn point. The ghost is updated by every clientbound packet
the proxy forwards. Because the ghost is what the client has, and not what some
server has, it stays correct across a switch.

At a switch the new server starts to describe the world to a connection it
believes belongs to a freshly joined player. The proxy does not forward that
description. It reads it into a second model, the *target view*, then compares
the two and sends the client the packets that turn the ghost into the target
view. If both servers run the same map, the comparison finds almost everything
equal, and the delta is small: some entities removed, some added, a few slots,
perhaps a different time of day. That delta is the entire visible event.

## 3. Connections

For each player there are two kinds of connection.

- The client connection, from the player to the proxy. The proxy answers the
  handshake, status and login itself, including encryption and compression, and
  from then on owns this connection for the whole session.
- A backend connection, from the proxy to a backend, opened by the proxy as if it
  were a client. There is one active backend connection and, during a switch, a
  second one that is being prepared.

The proxy authenticates the player against the session service once, at login.
Backends must not repeat it. They run in a mode where they accept the player's
identity from the proxy and from nobody else. Horizon servers already have that
mode: the modern player information forwarding that proxies of this kind use,
described in Horizon's `docs/velocity-forwarding.md`. The server runs with online
mode off and asks on the login channel `velocity:player_info`; the proxy answers
with a signature made with a shared secret, followed by the player's address,
unique id, name and profile properties, and the server refuses any login that does
not carry a valid answer. Meridian speaks this exchange as it is documented there,
so no new mode is needed in Horizon for identity. Backends are never reachable
from outside the private network. One point of that document is still to check:
profile properties (the skin) have not yet been tried against a real proxy, and
Meridian will be the first to send them.

### The configuration phase must match

A client leaves the configuration phase once and does not return to it.
Everything sent there (registries, tags, enabled features, resource packs, server
links) is fixed for the session. A switch is therefore possible only between
backends whose configuration is the same as the one the client received. The
proxy records a digest of what each backend sends in the configuration phase and
compares digests when a backend is added and again before each switch. A mismatch
makes the switch impossible, and the proxy refuses it or falls back to a visible
transfer, depending on configuration. For two copies of the same map the digests
should match, but that has to be checked, not assumed.

The same goes for what a backend puts in its Login packet, because the client
cannot change most of it later. Hardcore, the maximum number of players, whether
the server is in online mode, whether it enforces signed chat, and the dimension
type, the hashed seed and the flat and debug flags of the level can only be set by
a Login or a Respawn. They are part of the digest. What can change later (view
distance, simulation distance, game mode, immediate respawn) is sent in the delta.

## 4. The ghost

The ghost holds one section per kind of state. For each, the table says what is
kept and how the delta is made.

| State | Kept | Delta at a switch |
| --- | --- | --- |
| Chunks | Position of each loaded chunk, a hash of its data and of its light, the chunk cache centre and radius | Chunks with equal hashes are not sent again. Different chunks are sent as ordinary chunk packets. Chunks the target does not have are forgotten |
| Block changes | Folded into the chunk hash when a chunk is retained | None needed |
| Entities | Id, type, unique id, position, velocity, rotation, metadata, equipment, attributes, effects, passengers, leashes | An entity present in both views and equal is left alone. One present only in the ghost is removed. One present only in the target is added. A changed one gets the smallest movement, metadata or equipment packet |
| Players and tab list | Entries by unique id, game mode, latency, display name, list order | Entries removed and added as needed |
| Boss bars | By unique id, with title, health, colour, division, flags | Removed, added or updated |
| Scoreboard | Objectives, scores, teams, display slots | Removed, added or updated |
| Player status | Health, food, saturation, experience, level, abilities, game mode, held slot, effects | Packets only for what differs |
| Inventory and container | The inventory slots, the cursor stack, the open container and its slots, the state id | Slot updates for differences. An open container is closed by the switch in the first version |
| Time and weather | Game time, day time, whether the clock runs, rain and thunder levels | A time packet and weather events as needed |
| World | Border, difficulty and its lock, default spawn, tick rate and freeze | One packet per difference |
| Titles, action bar, sounds | Nothing kept | Whatever is playing finishes by itself |
| Recipe book, advancements | What the client has been told | Added and removed entries |

The delta is sent as a single bundle, so the client applies it in one tick. The
client already treats a bundle as indivisible; this removes the chance of drawing
a half-switched world.

The ghost costs memory, mostly in chunks. A hash per chunk and the packet bytes
are enough, since the proxy never needs to decode a chunk to compare it. The
proxy keeps the bytes only for the player's current view radius.

## 5. Switching

A switch has four phases. Only the last is visible to the client.

1. **Prepare.** The proxy opens a second backend connection for the same player
   and takes it through login and configuration. The target server creates a
   player in a held state: it does not simulate them, show them to others, or let
   them be hit. The target's first play burst (login, chunks, entities, inventory,
   position) is read into the target view and not forwarded. The proxy answers the
   target's teleport confirmation itself, since the client never sees that packet.
2. **Freeze.** The proxy stops forwarding the client's movement and actions to the
   old backend and queues them. It asks the old backend for the player's state
   (section 6). The client keeps predicting its own movement locally, which hides
   the pause.
3. **Hand over.** The old backend removes the player and returns the state. The
   proxy gives it to the new backend, which applies it to the held player. The new
   backend confirms and starts simulating them.
4. **Commit.** The proxy refreshes the target view with what changed since the
   prepare, builds the delta, sends it as one bundle, redirects the client's
   serverbound traffic to the new backend, replays the queued packets (rewritten),
   and closes the old connection.

The aim for the freeze is two or three ticks. The prepare phase can start early,
for example when the player gets close to a portal, so the work that takes time
(loading the player in, receiving chunks) is done before the player decides.

If any step fails, the proxy abandons the switch, discards the target view and the
held player, and unfreezes the old connection. A failed switch must leave the
player where they were.

## 6. What moves between servers

The part that belongs to the backends is the export and import of a player.
Meridian does not need the whole player file; it needs what continues the
player's life without a visible seam:

- Position, rotation, velocity, whether on the ground, fall distance, and the
  dimension.
- Health, food, saturation, exhaustion, air, fire and freezing ticks, invulnerable
  ticks, and active effects with the time they have left.
- Attribute base values and modifiers that are not derived from items.
- The inventory (main, armour, off hand), the held slot, the ender chest, the
  cursor stack. Item cooldowns are not saved by a vanilla server either, so they
  move only if the record gets a field of its own.
- Experience level, progress and total score.
- Game mode, abilities (flying, may fly, speed), and the respawn point.
- The recipe book and the advancements the player has.

What does not move: statistics, scores that belong to a server's own scoreboard,
and anything tied to something that exists only on the old server. Riding a
vehicle, or being ridden, is refused in the first version: the switch waits until
the player is on their own feet.

The record is a small versioned object that the proxy carries and mostly does not
open. Its main part is the player compound that Horizon already builds when it
saves a player, because both backends run the same Horizon version and that
compound is the most complete form there is, with the code to write and read it
already in place. Around it the record holds the few values the proxy checks for
itself (dimension, position), the advancements, and the live values that the save
path loses today. Times are relative ("this effect has nine seconds left") and
never absolute, because the two servers' clocks need not match.
[STATE-TRANSFER.md](STATE-TRANSFER.md) gives the field list, the gaps and the
requests.

The save path of a Horizon server has gaps that matter here: the velocity, the
food exhaustion, the freezing ticks, the previous game mode and the cursor stack
are not saved. A player who logs out and back in loses them as well, so they are
worth fixing in Horizon whatever Meridian does.

Transport is a control channel between the proxy and each backend, separate from
any game connection, with a request to expect a player, one to export, one to
import into a held player, and ones to release or drop. A Horizon server answers
requests like these only on a local control endpoint, so the recommended shape is
a small sidecar process next to each backend: it talks to the local endpoint and
offers the same requests to the proxy over TLS with a shared secret. Horizon then
needs no network change.

The work in Horizon is an export on demand, a held player, an import, and the
release requests. The login that trusts the proxy is already there (section 3), so
the earlier estimate of thirteen to nineteen days drops by the two or three days
that login was counted for, to about eleven to sixteen, most of it in the held
player, which every part of the server that
shows, tracks, ticks or hurts players has to skip. It lives in the game itself,
not in the supervisor, and should not be written until it has been agreed. This
document only describes the need.

## 7. Rewriting

The client sees one continuous server. The backends are unaware of each other, so
anything that is numbered per connection or per server has to be translated.

- **Entity ids.** Each backend numbers its entities from its own counter, so two
  backends will give the same id to different things. The proxy hands the client
  ids from its own space and keeps a map per backend. The player's own id is
  chosen once, at login, and stays; each backend's id for the player is mapped
  onto it. Every packet that carries an entity id (movement, metadata, equipment,
  passengers, leashes, damage events, animations, item pickup, and so on) is
  rewritten in both directions. The proxy parses those packets into the typed
  form from the protocol crate, changes the ids and writes them back. Packets that
  carry no id are passed through as bytes.
- **Keep-alive.** The proxy keeps the client alive on its own with its own ids,
  and keeps each backend alive separately. A client keep-alive is never forwarded,
  so a switch cannot leave a backend waiting for an answer that went elsewhere.
- **Teleport confirmations.** Each position correction from a server carries an id
  the client must answer. The proxy numbers them itself, remembers which backend
  each belongs to, and routes the answer. After a switch, an answer to an old id
  is dropped, not sent to the new server.
- **Containers.** Window ids and state ids belong to a server. The proxy keeps the
  client's view of both and translates clicks, so a click made against the old
  state is not applied to the new server. The plain inventory is resynchronised
  through the delta.
- **Sequence numbers.** Block interactions carry a sequence that the server
  acknowledges. The proxy keeps one counter for the client and maps it per
  backend, and acknowledges any pending sequence of the old server itself at the
  switch so the client does not wait on it.
- **Chunk batches.** The client reports how fast it takes chunks. The proxy
  forwards the reports to the active backend and answers batch markers of the
  inactive one.
- **Ping and pong.** Handled by the proxy.
- **Chat.** Signed chat links each message to the ones the player has already
  seen, and a switch breaks that link. The first version requires backends that do
  not enforce signed chat, and the proxy passes the player's chat session key to
  the new backend so that messages are still attributed to the right player.
- **Plugin channels and cookies.** Passed through. Cookies are stored by the
  client; the proxy does not interpret them.

## 8. The common map

The invisible switch depends on both servers describing the same world. There are
four ways to arrange that.

**A. Identical read-only copies.** Each backend loads its own copy of the same
world files, and nothing writes to them. Easiest, no coordination. Nothing built
or broken on one server shows on the other, and what players do is lost at
shutdown. It is enough to prove the switch.

**B. A shared directory.** Backends open the same world files. Two writers on one
region file corrupt it, and even one writer plus readers would see stale data. Not
acceptable.

**C. Replication of changes.** Each backend has its own copy, and changes to blocks
and block entities are sent to the other backends over the control channel as small
events, applied there, and ordered by the proxy or a log. Each server stays
consistent with the others within a short delay, there is no shared file, and each
backend can be restarted. It is the realistic way to a live shared map. Its cost is
writing the event stream and handling conflicts.

**D. One owner per area.** A server is the only writer of its part of the map, and
the others follow it. Players cross a boundary instead of choosing a server. This
is a different product, closer to a sharded world than to a proxy.

**Recommendation.** Use A for the first prototype and the first three milestones,
since they test the switch and not the map. Plan C as the next step, after the
switch is proven. Do not use B. Leave D aside.

The seed, the dimension keys and the registries must be the same on every backend.
The proxy checks the seed and the dimension at prepare time, next to the
configuration digest.

Under A, players on different backends do not see each other. For a lobby and two
game servers that is acceptable, since a game server only needs to show its own
players. Showing players across servers would mean the proxy copying entities
between target views, and is left for later.

## 9. Two modes

### Cooperative mode

When the client is Horizon, it says so in the configuration phase through the same
kind of announcement that mod messages use, and the proxy then uses a small
extension: a custom channel carried in play packets, understood only by a Horizon
client. A vanilla client ignores it.

The extension has few messages:

- *begin* (with a switch id): the client holds its own prediction state, because a
  server correction is about to come.
- *commit* (with the same id): follows the delta bundle. The client may now discard
  the state it held, such as pending block predictions and the sequence counter of
  the old server.
- *abort*: the switch was dropped.

What this gives over the vanilla path:

- The client knows when a switch starts and ends, and can clear state that no
  packet can reach, such as local block predictions, sounds started by the old
  server and particles that come from it.
- It makes the switch atomic from the client's point of view, with no dependence on
  how the packet arrival is split across ticks.
- It allows a few things plain packets do not: a different tick rate, other game
  rules, and a changed value in what the client reads at join.

The first working version is built here. This is the mode that is meant to be
reliable.

### Vanilla mode

A vanilla client gets the delta with no help. It works as far as the packets can
reach, and it is best effort. Artifacts that can show:

- Entities that exist on only one side appear or vanish at the switch.
- A very short pause in server corrections while the freeze lasts; movement
  continues locally but the world does not react.
- A visible position correction if the two servers disagree about where the player
  stands, which the held import is meant to prevent.
- Local block predictions made just before the switch may flicker when the new
  server does not confirm them.
- Sounds and particles from the old server finish playing.
- A broken signed chat link, unless chat is not enforced.
- No switch at all if the configuration digests differ, in which case the player
  gets a visible transfer or none.

## 10. Crates

One Rust workspace, with the protocol taken from `horizon-protocol` at a pinned
commit. Both projects are MIT licensed. The dependency is light: it brings in two
Horizon crates and no system library, and a first check of the workspace takes
about a minute (see [BUILD.md](BUILD.md)).

| Crate | Role |
| --- | --- |
| `meridian-config` | The proxy's configuration file: listeners, backends, routes, forwarding secret, switch rules, limits |
| `meridian-net` | Listeners and connections: handshake, status, login, encryption, compression, backend connections, timeouts |
| `meridian-ghost` | The ghost, the target view, and the delta between them. No I/O, so it is tested with recorded packet streams |
| `meridian-rewrite` | Entity id maps, teleport, container and sequence translation, and the rewriting of typed packets |
| `meridian-handoff` | The player record, the control channel client, and the prepare, freeze, hand over and commit steps |
| `meridian-ext` | The messages of the cooperative extension, shared with the Horizon client |
| `meridian-sidecar` | A small process run next to each backend: it relays the proxy's player requests to the backend's local control endpoint over TLS |
| `meridian-proxy` | The binary: wires the others together, runs the sessions, logs and metrics |
| `meridian-testkit` | A scripted fake backend and client, and a recorder of packet traces, for tests that need no real game |

The ghost and the rewriting are pure code over packets, so most of the hard logic
is testable with no network and no game.

## 11. Milestones

**J1: a transparent proxy.** A client connects to the proxy, the proxy logs in to
one Horizon server and relays everything in both directions, including status
pings and the player list. *Test:* a Horizon client and a vanilla client play for a
few minutes through it with no difference from a direct connection, and the packet
traces match.

**J2: the ghost.** The proxy decodes the clientbound stream and keeps the ghost.
*Test:* from a recorded session, the ghost at any point equals the state a client
holds, checked against a client's own state at the end of the same recording. No
change in what the player sees.

**J3: the switch between two test servers.** Two backends with the same map,
identical copies, and the switch triggered by a command. The proxy builds the
target view and the delta, rewrites ids, and moves the player. *Test:* with the
scripted client of the test kit, after the switch the client's state equals the
target backend's description, the old backend has no player, no packet from the old
backend arrives after the commit, and a failed switch leaves the player where they
were.

**J4: the proof, with no loading screen.** Two Horizon servers, a Horizon client
and the cooperative extension. *Test:* a recorded run of a switch in which no
Login, Respawn or configuration packet reaches the client, the client stays in
Play, and the screen capture shows no loading screen or black frame at the switch.
Then the same run with a vanilla client, with the artifacts of section 9 listed.

J4 is the only step that needs two servers and a client running together. Its cost
is planned before it is run. J1 to J3 need one or two servers, or none with the
test kit.

## 12. Decisions that are not ours alone

- **Changes in Horizon's game**: player export and import, and a held state for a
  joining player, about eleven to sixteen days of work in all. The login that
  trusts the proxy exists already (forwarding, section 3). The new work is needed,
  and it belongs to the Horizon project, so it must be agreed first. The switch
  itself, with no new connection and no Login, is the part nothing in Horizon
  provides today. The save gaps of section 6 can be fixed
  first and on their own.
- **Where the requests are served**: the sidecar of section 6 keeps Horizon free of
  a remote surface; the alternative is to let the supervisor's HTTP interface
  listen on a configured address, which is more work in Horizon.
- **Whose file is the truth**: after a switch the old server keeps a stale file for
  the player. Either the proxy is the only way in, or the release marks or removes
  that file.
- **A disconnect during a switch**: the record then exists only in the proxy, and
  must be imported into the target as a normal save or written back to the source.
- **A change in the Horizon client** for the cooperative extension, with the same
  caveat.
- **The common map**: the recommendation is A first and then C. Whether C is built
  as part of Meridian or as a feature of the Horizon server is open.
- **What to do when a switch is impossible**: refuse it, fall back to a visible
  transfer, or keep the player where they are. The configuration should allow each,
  and a default has to be chosen.
- **Players seeing each other across servers**: out of scope here, but it changes
  how the target view is built if it comes later.
- **Pinning the protocol crate**: Horizon has no stable protocol crate release, so
  the dependency is a pinned commit and moves when the project's version does. It
  starts pinned, and its build cost is small.
