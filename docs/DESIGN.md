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
- A seamless switch of a player between two backends, whether they run the same
  map or two different ones.
- A way for backends to hand a player's state to each other, with a policy that
  says which parts of it are kept (section 13).
- The ordinary functions of a proxy such as Velocity or BungeeCord: a list of
  named servers, routing, a command to change server, forwarding, status and the
  rest (section 14). The seamless switch is what Meridian adds to them.

Out of scope for now:

- The lobby. It will be a small spawn world served by an ordinary Horizon
  server, and the proxy treats it like any other backend.
- Switching between backends whose client-visible settings cannot change without
  a loading screen (section 8.2), or between versions of the game. A client speaks
  one protocol version, and the settings can fall back to an ordinary, visible
  transfer or be refused.
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
perhaps a different time of day. If the maps differ, the delta replaces the view
(section 8). Either way the delta is the entire visible event.

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
requests. Which parts of the record cross a given switch is a matter of policy
(section 13), so the import has to accept a partial record.

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

## 8. Different maps, and the common map

The switch does not need the two servers to share a map. A player can go from a
lobby to a game server whose world has nothing in common with it, and Meridian
still sends no Login, no Respawn and no configuration packet. What changes is the
size of the delta: with the same map it is small, with a different map it is the
whole view. A shared map becomes an optimisation, described at the end of this
section.

### 8.1 When the maps differ

The target view holds the target server's chunks, entities, spawn, time, weather,
border and difficulty, and the delta turns the ghost into it.

- **Chunks are replaced one by one, not cleared and refilled.** The proxy sends a
  new chunk and forgets the old chunk at the same position in the same bundle,
  nearest to the player first, so there is never a hole next to the player. Old
  chunks that have no replacement (the target's view is smaller) are forgotten
  last. A chunk's light travels with it.
- **The player's position is sent after the chunks around it.** The first bundle
  carries the chunk under the player and its neighbours, then the position. Whether
  a vanilla client waits for an unloaded chunk before it applies gravity was not
  verified, so the order is what keeps the player from falling through a world that
  has not arrived yet.
- **Entities are all removed and the target's are added.** The scoreboard, tab list
  and boss bars are replaced as in section 4.
- **The world values** (spawn, time, weather, border, difficulty, tick rate) are
  sent as the target has them.

What the player sees: no loading screen, no black frame, no Respawn. The world does
change, since it is another world. Terrain appears from the nearest chunks outward
over a short time, instead of behind a screen. That is the best a connection that
never leaves the Play state can do, and the document does not claim more.

**Cost.** A switch sends up to `(2r + 1)^2` chunks for a view distance `r`: 289 for
8, 625 for 12. A chunk packet after compression is somewhere between ten and a few
tens of kilobytes depending on the terrain, so a switch is in the order of a few to
a few tens of megabytes. This is an estimate, to be measured in J4 with two real
maps, not a figure to design on. The time is set by how fast the client takes
chunks, which it reports with its chunk batch feedback (section 7): at a few to a
few tens of chunks per tick the view completes in about one to five seconds. While
a switch is in progress the proxy holds the ghost and the target view, so its
memory use for that player doubles. Two settings can bring the cost down: a smaller
arrival radius that grows back after the switch (the client's radius can be changed
with a packet), and a limit on how many switches one player can make in a given
time.

### 8.2 What cannot change without a loading screen

Some values are set by the Login or the Respawn packet and no other packet can
change them, and the configuration phase is out of reach as well (section 3). A
server whose values differ from what the client already holds cannot receive this
player without a visible switch. For each, what goes wrong if it is ignored:

| Value | Set by | If the target differs |
| --- | --- | --- |
| Registries, tags, enabled features, resource packs | Configuration | The client misreads ids. Not tolerable |
| Dimension type (height, lowest block, light) | Login or Respawn | Chunk packets are cut into the wrong number of sections and fail. Not tolerable |
| Dimension key and sea level | Login or Respawn | Dimension-specific behaviour (compass, clock, portals, sky) follows the old value. Treated as part of the dimension: must be equal |
| Hardcore | Login | The client draws the wrong hearts and the wrong death screen. Cosmetic but wrong |
| Signed chat enforced | Login | Chat is rejected or the player is kicked. Not tolerable unless the proxy handles chat |
| Flat and debug flags | Login or Respawn | The horizon and void fog follow the old value. Cosmetic |
| Hashed seed | Login or Respawn | See below. Cosmetic and small |
| Maximum players | Login | Only a number in the client. Harmless |

The hashed seed deserves a closer look, because it is the one most likely to
differ between two different maps. The client keeps it as the zoom seed of its
biome manager, built once with the level, and uses it to jitter the position when it
picks the nearest biome cell for a block. The biomes themselves come from the
server inside each chunk, so they are right. Only the exact shape of the borders
between biomes is off, which shows as slightly different edges where the grass,
foliage and water tints blend, and in the biome used for ambient sound and
particles at a border. This is read from the structure of the client's classes and
has not been seen on screen; J4 checks it. The recommendation is to allow it by
default.

The dimension also constrains the arrival point. The client is in one dimension,
so the player must arrive in the same one, whatever the policy of section 13 says
about position.

**When the servers are incompatible**, the route decides, and the choice among
these is still open (section 12):

- *stay*: refuse the switch and tell the player;
- *visible*: do the ordinary switch, which sends Start Configuration, runs the
  target's configuration phase, and then its Login, with the loading screen that
  goes with it;
- *fallback*: send the player to a server listed as the fallback instead.

Each value above has its own setting: `require` (must be equal), `warn` (switch
and log) or `ignore`. The defaults, decided in section 12, are `require` for the registries, the
dimension and the signed-chat setting, and `ignore` for the hashed seed, hardcore,
the sea level and the flat flag.

### 8.3 Sharing a map as an optimisation

When two servers do serve the same world, equal chunks are recognised by their
hash and not sent, and a switch costs almost nothing. There are four ways to
arrange a shared map.

**A. Identical read-only copies.** Each backend loads its own copy of the same
world files, and nothing writes to them. No coordination. Nothing built or broken
on one server shows on the other.

**B. A shared directory.** Backends open the same world files. Two writers on one
region file corrupt it. Not acceptable.

**C. Replication of changes.** Each backend has its own copy, and changes to blocks
and block entities are sent to the other backends over the control channel as small
events and applied there. Consistent within a short delay, no shared file.

**D. One owner per area.** A server is the only writer of its part of the map and
the others follow it. A different product, closer to a sharded world.

None of these is needed for the switch to work. A is the simplest way to test the
cheapest case, two different maps are the case that tests the whole mechanism, and
C stays an option for a lobby and games that really share a world. B is excluded
and D left aside. The checks of section 8.2 apply whether or not the maps are
shared.

Players on different backends do not see each other. Showing them across servers
would mean the proxy copying entities between target views, and is left for later.

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
- With different maps, terrain that fills in from the nearest chunks outward over a
  second or more, and slightly different biome borders if the hashed seed differs
  (section 8).
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
| `meridian-policy` | The retention profiles of section 13, their evaluation on a record, and the traits where extensions plug in |
| `meridian-auth` | The login modes of section 15: the session check, the name lookup with its cache, and the marking of players as verified or unverified |
| `meridian-proxy` | The binary: wires the others together, runs the sessions, logs and metrics |
| `meridian-testkit` | A scripted fake backend and client, and a recorder of packet traces, for tests that need no real game |

The ghost and the rewriting are pure code over packets, so most of the hard logic
is testable with no network and no game.

## 11. Milestones

**J1: a transparent proxy.** A client connects to the proxy, the proxy logs in to one
Horizon server with Horizon's forwarding and relays everything in both directions,
including status pings and the player list. Named servers, a default server,
compression, timeouts and a fixed login mode (`online`, or `offline` for tests) come
with it. *Test:* a Horizon client and a vanilla client play for a few minutes
through it with no difference from a direct connection, and the packet traces match.

**J2: the core of a proxy.** The functions marked J2 in section 14: routing with a
fallback list and forced hosts, the command to change server (an ordinary, visible
switch through Start Configuration), the console and its commands, reload,
aggregated status, limits, and the hybrid login with groups of backends (section 15).
*Test:* with several test servers, a player is routed by hostname and by login kind,
changes server by command, and is sent to the fallback when their server stops.

**J3: the ghost.** The proxy decodes the clientbound stream and keeps the ghost.
*Test:* from a recorded session, the ghost at any point equals the state a client
holds, checked against a client's own state at the end of the same recording. No
change in what the player sees.

**J4: the switch between two test servers with different maps.** Two backends whose
maps have nothing in common, a same-map pair as the cheapest case, and the switch
triggered by a command. The proxy builds the target view and the delta, rewrites
ids, replaces the chunks one by one, and moves the player. *Test:* with the
scripted client of the test kit, after the switch the client's state equals the
target backend's description, the old backend has no player, no packet from the old
backend arrives after the commit, and a failed switch leaves the player where they
were. Servers whose dimension type or registries differ are refused or sent through
the visible path, as configured, and a different hashed seed is allowed. The run
records bytes and time for the switch, so the estimates of section 8.1 are replaced
by measures.

**J5: the proof, with no loading screen.** Two Horizon servers with different maps,
a Horizon client and the cooperative extension. *Test:* a recorded run of a switch
in which no Login, Respawn or configuration packet reaches the client, the client
stays in Play, and the screen capture shows no loading screen or black frame at the
switch; the biome borders after the switch are looked at, for section 8.2. Then the
same run with a vanilla client, with the artifacts of section 9 listed.

**J6: what is kept.** Profiles of section 13 in the configuration, applied at every
switch, with the modes `carry`, `target` and `reset` for each block. *Test:* a
player with a full inventory, effects and experience goes through a route that
carries some blocks, resets others and takes the rest from the target, and each
ends up as the profile says.

**J7: extensions.** Named opaque blocks, messages between servers, the extension
trait on the proxy, bans and richer permissions, and metrics.

J4 and J5 depend on the Horizon features of section 16 (export, import and the held
player). Until they exist, J4 is developed and tested against the scripted backends
of the test kit, which answer the same requests as the sidecar would. J5 is the
only step that needs two servers and a client running together, and its cost is
planned before it is run. J1 to J4 need one or two servers, or none with the test
kit.

## 12. Decisions

These were open questions for the project's owner. They were answered on
2026-11-02, accepting the recommendation of this document for each. A decision can be
reopened; until then it is the rule.

1. **Changes in Horizon's game** (export and import of a player, a held state, hook
   points; section 16). *Decided 2026-11-02:* the work is done in Horizon only if
   its maintainers accept it. A question to them is open as
   horizon-rust/horizon#7484. The save gaps of section 6 can be fixed first and on
   their own. The login that trusts the proxy exists already (forwarding,
   section 3).
2. **A change in the Horizon client** for the cooperative extension. *Decided
   2026-11-02:* later, after the switch works with plain packets.
3. **When a switch is impossible.** *Decided 2026-11-02:* *stay* (refuse the switch
   and tell the player), settable per route to *visible* or *fallback*
   (section 8.2).
4. **Which client-visible differences are tolerated.** *Decided 2026-11-02:* the
   registries, the dimension (type and key) and the signed-chat setting must be
   equal. A different hashed seed, hardcore, sea level and flat flag are tolerated.
5. **What is kept by default** (section 13). *Decided 2026-11-02:* inventory, ender
   chest, experience, health, food and effects are carried; game mode, position,
   advancements and statistics are taken from the target; unknown named blocks are
   kept in the proxy's copy.
6. **The form of the proxy's extension point.** *Decided 2026-11-02:* a Rust trait
   first, a local socket later if asked, no dynamic libraries.
7. **Login modes** (section 15). *Decided 2026-11-02:* `online` is the default;
   `offline` and `hybrid` are included; the documentation says *verified* and
   *unverified*.
8. **Where the player requests are served.** *Decided 2026-11-02:* a sidecar next to
   each backend (section 6).
9. **Whose file is the truth after a switch.** *Decided 2026-11-02:* the proxy is the
   only way in; backends are private, so a stale file on the old server is never read
   without the proxy.
10. **A disconnect during a switch.** *Decided 2026-11-02:* the record is imported
    into the target as a normal save.
11. **A shared map** (section 8.3, option C). *Decided 2026-11-02:* not built for
    now.
12. **Players seeing each other across servers.** *Decided 2026-11-02:* not
    supported; players on different backends do not see each other.

One earlier decision stands with them: the protocol crate is a pinned commit of
Horizon (section 10), kept pinned because its build cost is small.

## 13. What is kept

Between two servers that do not share a map, a player's state is not obviously
something to carry. A lobby does not want the inventory of a game, a game may want
to start every player with the same kit, and a rank belongs to the player on every
server. So what crosses a switch is a policy, chosen by the operator, and not a
fixed list.

### 13.1 The record and the blocks

For the length of a session the proxy holds a copy of the player's data, the
record of section 6, in memory. At a switch the source exports it, the proxy applies
the policy to it, and the target imports the result. The proxy does not write
player files; the servers do. A disconnect in the middle of a switch is covered in
section 12.

The record is split into *blocks*, and the policy speaks about blocks:

| Block | What it holds |
| --- | --- |
| `inventory` | Main inventory, armour, off hand, held slot |
| `ender_chest` | The ender chest |
| `experience` | Level, progress, total |
| `health` | Health and absorption |
| `food` | Food, saturation, exhaustion |
| `effects` | Active effects with their time left |
| `game_mode` | Game mode and abilities |
| `position` | Where the player stands, and their velocity |
| `spawn_point` | The respawn point |
| `recipe_book` | The recipe book |
| `advancements` | The advancements |
| `statistics` | The statistics |
| `namespace:name` | A named block that is opaque to the proxy (13.3) |

### 13.2 Modes and profiles

Each block has one of three modes:

- `carry`: the source's value goes to the target.
- `target`: the player gets what the target already has for them, its own saved
  value if they have been there before and its default if they have not. The proxy
  leaves the block out of the import.
- `reset`: the player gets the blank value (an empty inventory, zero experience,
  full health).

`position` has its own forms: `carry` keeps the coordinates (valid only when the
target accepts them and the dimension is the one the client is in, section 8.2),
`target` uses the target's saved position or its spawn, and an arrival point names
coordinates and a dimension. Because the client cannot change dimension without a
loading screen, an arrival point in another dimension is a configuration error.

A *profile* is a named set of modes. Profiles are written in the TOML
configuration, and a route says which profile applies to a move:

```toml
[retention.default]
inventory = "carry"
ender_chest = "carry"
experience = "carry"
health = "carry"
food = "carry"
effects = "carry"
game_mode = "target"
position = "target"
advancements = "target"
statistics = "target"
unknown_blocks = "keep"

[retention.lobby_to_game]
inventory = "reset"
effects = "reset"
position = { arrival = "0 64 0", dimension = "minecraft:overworld" }

[retention.default.blocks]
"ranks:rank" = "carry"

[[route]]
from = "lobby"
to = "game-1"
retention = "lobby_to_game"
```

When several profiles could apply, the most specific wins: a profile for the pair
of servers, then a profile for the destination, then the default. A profile starts
from the default and changes only the blocks it names. The names above are a
proposal, not a fixed format.

### 13.3 Opaque blocks

Data that a vanilla player file does not hold, such as a rank or a currency
balance, belongs to a plugin of the server and is not in the compound. For these the
record carries *named blocks* keyed by `namespace:name`, with bytes the proxy never
reads. A block has a size limit (a proposal: 64 KiB), a version number that its
owner writes, and a mode like the others. A block the target has no reader for is
dropped or, with `unknown_blocks = "keep"`, kept in the proxy's copy for the rest of
the session, so that it comes back if the player returns to a server that reads it.

### 13.4 What exists, and what is missing

What exists in Horizon, as far as it was read:

- The player compound that the server saves and loads. Keys Horizon does not model
  survive a load and a save, as [STATE-TRANSFER.md](STATE-TRANSFER.md) found. It
  was not checked whether a mod can write into it.
- Native mods with server-side hook points on a dedicated server: world lifecycle,
  server ticks, blocks, sounds, a file of their own under the server's folder
  (`docs/native-mods.md`).
- Mod messages between the server half and the client half of one mod (the
  `horizon:mod_net` channel). They connect a server and its own players, not two
  servers.
- Issue #6774, open, which proposes client-side hook points for custom payloads on
  a mod's own channel. It is aimed at client mods talking to a server's plugin, and
  would not carry anything from one server to another.

What is missing:

1. In the game: export and import of a player, with an import that accepts a
   partial record (the `target` mode leaves blocks out).
2. A pair of hook points that let a mod add named blocks to an export and read
   them at an import. The shape proposed is one call at export, with the player and
   a list to push `(name, bytes)` pairs into, and one at import with the blocks the
   mod owns.
3. The sidecar's requests carrying the named blocks.
4. On the proxy: the policy engine and its TOML, and an extension point (13.5).

Nothing in this section is accepted by Horizon. It describes what Meridian would
need, to be proposed there according to that project's own process (section 15).

### 13.5 An extension point on the proxy

A plugin sometimes needs to act on the proxy itself: to change a policy for one
player, to refuse a move, to add a block no server produces, to choose a target.
The proxy exposes points where such code runs: before a switch is decided, after the
export, before the import, and after the commit. Each sees the record and the
players involved and can change the record or refuse the move.

Two forms are possible. A Rust trait that built-in policies and anyone building the
proxy from source can implement, and a local socket protocol, like the sidecar's,
that lets a separate process do the same in any language. The recommendation is to
start with the trait, which is cheap and gives the policy engine a clean shape, and
to add the socket later if people ask for it. Loading plugins as dynamic libraries
is not proposed.

## 14. What a proxy such as Velocity or BungeeCord does, and where Meridian stands

The aim is a proxy that does what those two do, built only for Horizon and in Rust
for speed, with the seamless switch on top. This section lists the functions such a
proxy is known for and says when each arrives, or that it is out of scope.

**Where this list comes from.** It is made from what the two projects describe
publicly to their users, the names of their settings and commands and their
documented behaviour, and from what a proxy of this kind is seen to do. No source
code of Velocity or BungeeCord has been read or copied for Meridian, and none will
be. Before a function is built, it is specified from the user documentation and from
observed behaviour, then checked against Horizon, the same way Horizon's own
forwarding was written. The milestones are those of section 11.

### Core

| Function | In Meridian | Milestone |
| --- | --- | --- |
| Named servers | A list of backends with a name and an address, read from the configuration | J1 |
| Default server | Where a new player lands | J1 |
| Player information forwarding | Horizon's existing exchange (section 3), with a shared secret | J1 |
| Online mode | The proxy checks the player with the session service; backends run with it off | J1 |
| Compression | A threshold set in the configuration, applied on both sides | J1 |
| Connect and read timeouts | Set in the configuration | J1 |
| Status reply | The proxy's own message of the day, or one passed through from a chosen backend | J1 |
| Aggregated status | Online count summed over the backends, the shown maximum set in the configuration | J2 |
| Player limit | A maximum enforced at login, apart from the number shown | J2 |
| Fallback list | An ordered list ("try") used when the default server is down or a player's server stops | J2 |
| Forced hosts | Routing by the hostname the player typed | J2 |
| Command to change server | `/server <name>` for a player, with the names completed, as an ordinary visible switch first | J2 |
| Moving others | A console or permitted command that sends one player, or everyone on a server, elsewhere | J2 |
| Listing players | Commands that list the players, per server and in all, and find one | J2 |
| Proxy console | Commands to list, switch, send, reload, show the version and stop | J2 |
| Reload | The configuration is read again without a restart | J2 |
| Backend down | Players on a backend that stops are sent to the fallback, or told why | J2 |
| Duplicate login | A second login of the same player replaces the first or is refused, as set | J2 |
| Login rate limit | A limit on connections per address in a given time | J2 |
| Proxy protocol | Reading the real address from a front balancer that sends it | J2 |
| Basic permissions | A short file that says who may use the proxy's commands | J2 |
| Logging | Readable logs, with a level setting | J2 |

### The seamless switch

| Function | In Meridian | Milestone |
| --- | --- | --- |
| Ghost of the client's state | Section 4 | J3 |
| Seamless switch, the proxy's own command and API | Sections 5 to 8, between test servers first | J4 |
| Proof on screen with a Horizon client | Section 9, cooperative mode | J5 |
| Policy for what is kept | Section 13, with profiles in the configuration | J6 |
| Data of plugins between servers | Section 13.3, with the hooks proposed in section 16 | J7 |

### Extensions

| Function | In Meridian | Milestone |
| --- | --- | --- |
| Events and an extension API | Rust trait on the proxy (13.5) | J7 |
| Messages between servers | Named channels carried through the proxy and the sidecars | J7 |
| Ban lists at the proxy | Bans by name, unique id and address that apply before any backend | J7 |
| Richer permissions | Groups and per-command permissions, through the extension API | J7 |
| Metrics | Counters of players, switches and timings, in a text format | J7 |

### Out of scope

- Plugins written in Java, or a Java API compatible with theirs.
- Legacy forwarding variants and the shared-secret scheme of BungeeCord's older
  mode: Horizon already reads one form, and that is the one Meridian speaks.
- Versions of the game other than the one Horizon serves, and modded handshakes.
- A tab list merged across servers, global chat, queues and load balancing between
  copies of one server, and a remote console: these are plugin matters.
- Sending resource packs from the proxy. A pack comes with the configuration
  phase, which belongs to the backend (section 3).
- The query protocol of the server list.

The core is what comes first: the server list, routing, the command to change
server and forwarding. The ghost and the switch follow, because they depend on the
relay being sound.

## 15. Login: verified and unverified players

A proxy can log a player in in two ways: it can check their account with the
session service, or it can accept a name it cannot check. This section describes
both, how the proxy chooses, and how players of each kind are kept apart.

### 15.1 The proxy does the checking

Backends always run with online mode off, behind Horizon's forwarding (section 3).
It is the proxy that authenticates. For a *verified* player the sequence is the
usual one: the client sends its name, the proxy sends an encryption request, the
client answers it after telling the session service that it is joining, the proxy
asks the session service to confirm, and from then on the connection is encrypted.
The proxy then marks the player *verified*, or *unverified* when it let a name
through without that check, and routes by the mark.

This changes what "online" and "offline" servers mean. They are no longer a setting
of each server. All backends are the same, and the operator defines *groups* of
backends, one for each kind of player. The proxy sends a verified player to a server
of the verified group and an unverified player to a server of the unverified group.

### 15.2 Deciding before the encryption request

A client that has no account to check cannot answer an encryption request, so the
proxy has to decide, from the name alone and before it asks, whether to ask. Three
modes:

- `online`: always ask. A client that cannot answer is refused. This is the
  default.
- `offline`: never ask. Every player is unverified. Nothing proves who anyone is, so
  this suits a closed network (a LAN, private tests) and nothing else.
- `hybrid`: look the name up with the account service's public profile lookup.
  If an account with that name exists, ask for encryption, and refuse a client that
  cannot answer. If none exists, let the player in as unverified. The lookup has a
  cache (a positive answer is kept longer than a negative one), a timeout, and a
  limit per address. `on_lookup_failure` says what happens when the service does not
  answer: `refuse` (the recommendation, since a name might be an account's) or
  `unverified`.

The limits of the hybrid mode are known and should be stated to whoever turns it
on:

- Nobody can take over a name that exists as an account: a client that claims one
  and cannot authenticate is refused.
- A player without an account whose chosen name also exists as an account is
  refused as well. The proxy has no way to tell them from someone claiming the name.
  A list of exceptions would reopen the impersonation it prevents, and login
  passwords would be a feature of their own, so both are outside this design.
- A free name today can become an account tomorrow, and the same person is then
  asked to authenticate. Their unverified identity does not carry over.
- The lookup is one more call at every login, and it can be slow or unavailable.

### 15.3 The unique id

A verified player has the unique id of their account. An unverified player has an
id derived from the name, the way a server with online mode off derives it. The two
are different for the same person, and a server keeps files by this id, so the two
kinds must not meet in one group: they would have two files, and the ranks and
inventories of one would not be seen by the other. The configuration therefore
refuses a backend that is listed in two groups of different kinds, and the profile
the proxy keeps for a player says which kind of id it is. Moving a player between
groups of different kinds is refused by default, and the retention policy of
section 13 means nothing across them.

### 15.4 Configuration

```toml
[auth]
mode = "hybrid"                # online | offline | hybrid
on_lookup_failure = "refuse"   # refuse | unverified
lookup_cache_seconds = 3600

[groups.verified]
servers = ["lobby", "game-1", "game-2"]
default = "lobby"

[groups.unverified]
servers = ["lobby-open"]
default = "lobby-open"
```

With `mode = "online"` only the verified group is needed, and with
`mode = "offline"` only the unverified one. The names and keys are a proposal.

### 15.5 Position

Taking unverified players is a choice for whoever runs the proxy, and the default
refuses them. Horizon the game asks each player for their own copy of the game and
prepares it from there (Horizon's documentation on preparation from the player's own
copy); Meridian carries no game content and says nothing about how to obtain any.
The word used here and in the configuration is *unverified*, because it describes
what the proxy knows: it could not check the account. It is not a way around
anything. An unverified name proves nothing, so groups for unverified players
should carry no privileges, and bans and permissions that depend on a name are weak
for them.

Milestones: a fixed mode (`online`, or `offline` for tests) comes with the relay in
J1; the hybrid mode and the groups come in J2.

## 16. Work proposed to Horizon

Several things in this design would have to exist in Horizon's own code. Each is
listed here as a proposal, as a unit that could become an issue of its own. None
has been opened as an issue, and none has been accepted by the Horizon project.
A question to its maintainers about whether they would take this work is open as
horizon-rust/horizon#7484. If one is taken up, it follows that project's way of working (an issue, a claim through its
maintainers, and its own pull request conventions), not the rules of this
repository.

| # | Proposal | Stands on its own |
| --- | --- | --- |
| 1 | Save the values the player file loses today: velocity, food exhaustion and tick timer, freezing ticks, previous game mode, and put the cursor stack back in the inventory when a player leaves. A player who logs out and in loses them already | Yes |
| 2 | Export one connected player on demand: close the menu, freeze them, answer with the record of section 6 | Needs 1 |
| 3 | A held player: connected and receiving the world, but not simulated, shown, announced or hurt, entered by an "expect this player" request | Yes, but large |
| 4 | Import a record into a held player, partial records included, and release it | Needs 2 and 3 |
| 5 | Release and drop requests: remove a player after a switch without saving or announcing, abort a switch, drop a held player | Needs 3 |
| 6 | Hook points for mods at export and at import, to add and to read named blocks | Needs 2 and 4 |
| 7 | A note, and a test if possible, on forwarding with profile properties (the skin), which the forwarding document says were not observed against a real proxy | Yes |

The sidecar, the policy engine, the ghost and the proxy are Meridian's own and need
nothing from Horizon beyond these. The compatibility of two servers is learned by a
probe connection that reads their configuration and Login, so no request is needed
for it.
