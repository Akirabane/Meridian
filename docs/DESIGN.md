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
identity from the proxy and from nobody else. The recommended form is a signed
statement of name, unique id and profile properties, passed in the login phase
with a secret shared between proxy and backends, and backends that refuse any
login without it. Backends are never reachable from outside the private network.

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
  cursor stack, and item cooldowns.
- Experience level, progress and total score.
- Game mode, abilities (flying, may fly, speed), and the respawn point.
- The recipe book and the advancements the player has.

What does not move: statistics, scores that belong to a server's own scoreboard,
and anything tied to something that exists only on the old server. Riding a
vehicle, or being ridden, is refused in the first version: the switch waits until
the player is on their own feet.

The format is a small, versioned record, not a copy of the player file, so the two
servers can differ in how they store it. Times are relative ("this effect has nine
seconds left") and never absolute, because the two servers' clocks need not match.

Transport is a control channel between the proxy and each backend, separate from
any game connection: authenticated with a shared secret, over TLS, with a request
to export a player, a request to import one into a held player, and a request to
release it. Horizon's server already has a control protocol of this shape, and
extending it is the likely route.

This needs a feature in `horizon-server`: player export and import, a held state
for a joining player, and the proxy-trusting login mode. It touches the server and
should not be written there until it has been agreed. This document only
describes the need.

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

