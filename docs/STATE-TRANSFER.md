# Moving a player's state between Horizon servers

Section 6 of [DESIGN.md](DESIGN.md) says what has to move when Meridian switches a
player from one backend to another, and that the backends have to do the export
and the import. This document looks at the server side: what a Horizon server
already does with a connected player's state, what is missing for a switch with
no visible seam, and a small server API that would close the gap.

It was written by reading Horizon's source, without changing it. Every claim
names a file and line in the `horizon-rust/horizon` repository at commit
`c3905131b`, or a class of the vanilla 26.3 server read with `javap`. A claim
that could not be checked that way is marked *not verified*. Line numbers move
with every commit; the names of the functions are the stable part.

Section 2.6 and what depends on it (5, 6.2, 6.4, 7) were brought up to date with
commit `02c8607ee`: Horizon gained Velocity forwarding after `c3905131b`.

Paths are relative to `crates/horizon-client/src/` in the Horizon repository.
A path that starts with `horizon-server/` or `horizon-protocol/` is relative to
`crates/`.

## 1. How a Horizon server holds a player

A Horizon dedicated server is the game itself running with no local player.
`horizon-server` starts the game binary with `--dedicated --nogui` and a control
endpoint (`horizon-server/src/engine.rs:233-260`). The game then opens its world
and serves the network through the same code that opens a single player world to
the LAN (`game/dedicated.rs:842`, `lan_host`). Every connected
player is a *guest* of that LAN host. The dedicated server explicitly drops the
single player's own state, because players have their own files
(`game/dedicated.rs:713-714`).

So the state of a connected player lives in three places:

- **The guest** (`lan/guest.rs:63-306`, `struct Guest`):
  position, rotation, game mode, inventory, armour, off hand, cursor stack,
  ender chest, health, food, saturation, experience, effects, the open menu,
  the respawn point, the advancements in memory, the recipe book, and the
  untouched player compound read from disk (`saved`, line 285).
- **The guest's body** (`motion: Option<Box<PlayerMotion>>`, `lan/guest.rs:195`):
  the simulated player, with velocity, fire ticks, air, fall distance, absorption
  and food exhaustion. The `lan status` request reads air, absorption and
  exhaustion from it (`game/lan.rs:1166-1173`).
- **The host's per-connection play state** (`host.play`, keyed by connection):
  the statistics (`game/lan.rs:1454-1457`), the shield cooldown and the item in
  use (`game/lan.rs:1155-1159`), and the open crafting table or workstation
  (`game/lan_crafting.rs:640-647`).

## 2. What already exists

### 2.1 A complete save and load of the player compound

`lan/playerdata.rs` saves and loads a guest in the vanilla
player compound, through the same code that saves the single player:

- `store` (`playerdata.rs:148-229`) copies a live guest into the player fields
  of a `Level`. `save` (`:134-145`) then writes `players/data/<uuid>.dat`.
- `load` (`:42-56`) reads that file and `apply` (`:59-131`) gives its state to a
  guest.
- `Level::player_tag` (`save.rs:3645-3870`) builds the
  compound by starting from the compound that was loaded and writing Horizon's
  own fields over it, so keys Horizon does not model survive a load and a save.
  `Level::read_player` (`save.rs:3510-3643`) is the reverse.

Advancements and statistics are separate JSON files, written by
`save_guest_records` (`game/lan.rs:1433-1458`) and read by `load_guest_records`
(`game/lan.rs:1462-1490`), as vanilla's `PlayerAdvancements` and
`ServerStatsCounter` do.

This is most of an export and an import already. What is missing is a way to
run it on demand, for one player, without the player leaving, and to take the
result from memory instead of a file (section 5).

### 2.2 A join that can take its state from memory

When a guest joins, `lan_guest_joined` (`game/lan.rs:1964-2053`) picks the saved
state. For the owner of an integrated server it takes a `Level` held in memory
(`owner.saved`, `game/lan.rs:1980-1987`); for everyone else it reads the file
(`playerdata::load`, `:1988`). It then places the player with
`Hub::place_new_player` (`lan/hub.rs:865-961`).

The in-memory branch is the precedent an import needs: the same `apply`, fed
from a record the proxy sent instead of a file.

### 2.3 A leave that saves

When a guest leaves, the host puts the contents of their crafting grid and
workstation back into their inventory (`lan_crafting_left`,
`game/lan_crafting.rs:634-660`), saves them (`save_guest`,
`game/lan.rs:1375-1386`, called at `:1876`), and forgets their play state
(`:1877-1879`). The hub tells the other players that they left
(`remove_guest`, `lan/hub.rs:1497-1530`).

### 2.4 A local control endpoint

The game answers text requests on a control endpoint. A request is one line,
the answer one JSON line (`control.rs:1-11`, `serve` at
`:344-365`). On Windows, or when the path ends in `.json`, it is TCP on
`127.0.0.1` with a random token that the first line must present
(`control.rs:191-211`, `authenticate` at `:301-324`). `horizon-server` always
uses the TCP form (`horizon-server/src/engine.rs:262-265`).

A dedicated server answers two requests there (`game/dedicated/status.rs:40-59`):
`server-status` and `console <line>`, which runs a console command and returns
the lines it logged. The requests are answered on the server thread between
ticks (`poll_server_control`, called at `game/dedicated.rs:917`), so a request
sees the world at a tick boundary.

`horizon-server` itself has an HTTP API with a bearer token, also bound to
`127.0.0.1` (`horizon-server/src/api.rs:442-483`), whose `POST /api/v1/command`
forwards to `console` above (`horizon-server/src/api.rs:512-527`).

Both are local only. A proxy on another machine cannot reach either of them
today.

### 2.5 An export through the console, but no import

`/data get entity <player>` already works on a guest: it runs the player's
compound through `player_tag` (`game/commands/data.rs:40-71`). Sent as
`console data get entity <name>`, it returns the player's compound as SNBT text.
That is an export, but only as a chat message to be parsed, without the
advancements, and without the live values listed in section 3.

Writing to a player is refused, as in vanilla: `apply_entity_nbt` rejects the
player (`data.rs:156`) and anything it does not handle, guests included
(`data.rs:198`). Vanilla's `EntityDataAccessor.setData` refuses any `Player`
with `commands.data.entity.invalid` (javap). There is no import.

### 2.6 Login

In online mode the server authenticates the player with the session service;
in offline mode it derives the UUID from the name (`lan/conn.rs:717-734`).

At `c3905131b` that was all. Since #4963 (commit `5b2537191`, on `main` after
`c3905131b`), a dedicated server can also trust a proxy, with Velocity's
"modern" player information forwarding, version 1. The paths and lines below are
those of commit `02c8607ee`.

- The server turns it on when a file `forwarding.secret` sits in its folder and
  `online-mode` is off (`game/dedicated.rs:233-258`, read at `:914`, kept in
  `NetConfig::forwarding_secret`, `lan/mod.rs:182`).
- At login it sends the proxy a login plugin request on `velocity:player_info`
  (`forward`, `lan/conn.rs:825`). The answer is an HMAC-SHA256 signature made
  with the shared secret, then the player's real address, UUID, name and profile
  properties (`lan/forwarding.rs:17-37`, and Horizon's `docs/velocity-forwarding.md`).
- The server takes that profile instead of the one it would derive
  (`lan/conn.rs:738-771`). A login without a valid answer is refused with
  "This server is behind a proxy: join through the proxy." (`:772`).

So the identity part of section 6.4 already exists. Horizon's own notes say the
encoding of non-empty properties (the skin) was never checked against a real
proxy (`docs/velocity-forwarding.md`, "Not observed"); Meridian would be the first
to send them.

## 3. Field by field

What section 6 of DESIGN.md asks for, against what the save path carries today.
"Carried" means `store` and `player_tag` write it and `read_player` and `apply`
read it back, so a save on one server and a load on another keep it.
`playerdata.rs` is `lan/playerdata.rs`; `guest.rs` is `lan/guest.rs`.

| State | Where it lives | Carried today | Note for a switch |
| --- | --- | --- | --- |
| Position, dimension | guest | Yes (`playerdata.rs:149-150`) | |
| Rotation | guest | Yes (`playerdata.rs:151`); head yaw is set to the yaw on load (`:66`) | |
| Velocity | body | **No.** `store` never sets the `Level`'s motion, so `Motion` is written as zero (`save.rs:3720`), and `apply` does not read it | Needed: without it the player stops dead at the switch |
| On ground, fall distance | guest, body | Yes (`playerdata.rs:172`, `:181-187`) | |
| Health, absorption | guest, body | Yes (`playerdata.rs:152-158`) | |
| Food, saturation | guest | Yes (`playerdata.rs:159-160`) | |
| Food exhaustion, food tick timer | body | **No.** `store` leaves them at the `Level` default and `player_tag` writes that (`save.rs:3727-3731`), so they reset to 0 | Small: a hungry player regains a little food time |
| Air, fire ticks | body | Yes, through `SavedBody` (`playerdata.rs:181-196`, `lan/guest.rs:27-47`) | |
| Freezing ticks | body | **No.** The body does freeze: `PlayerMotion.ticks_frozen` (`motion.rs:395-396` at `549d6b9cd`) rises by one a tick in powder snow (`:1606-1607`) and falls by two outside it (`:1291`, `:1451`). But `SavedBody` has no field for it (`guest.rs:27-31`), and nothing in `playerdata.rs`, `guest.rs` or `save.rs` writes it, where vanilla saves `TicksFrozen` for every entity, players included, when it is above 0 (`Entity.saveWithoutId`, javap) | Small |
| Ticks of invulnerability after a hit | body | No (*not verified* whether vanilla saves them) | Small; can be dropped |
| Active effects | guest | Yes, with the ticks left (`playerdata.rs:165`, `save.rs:3737-3754`) | Already relative, as DESIGN.md wants |
| Attributes | body | Yes, the body's attribute map (`playerdata.rs:197-206`) | Whether modifiers that come from equipped items are left out: *not verified* |
| Inventory, armour, off hand, held slot | guest | Yes (`playerdata.rs:169`, `:207-228`) | |
| Ender chest | guest | Yes (`playerdata.rs:208`) | |
| Cursor stack | guest (`guest.rs:214`) | **No.** Not in `store`, and not in `lan_crafting_left` either (`game/lan_crafting.rs:634-660`). Vanilla puts it back into the inventory when a menu closes (`AbstractContainerMenu.removed` calls `dropOrPlaceInInventory`, javap). Whether Horizon does that elsewhere when a guest leaves: *not verified* | The export must put it back into the inventory, or carry it |
| Crafting grid, open workstation | guest, play state | Put back into the inventory when a guest leaves (`game/lan_crafting.rs:634-660`) | The export must do the same first |
| Open container | guest (`guest.rs:280`) | No | DESIGN.md closes it at the switch in the first version |
| Item cooldowns | play state | No. Vanilla does not save them either: no cooldown key in `Player.addAdditionalSaveData` (javap). Horizon keeps the shield cooldown per connection (`game/lan.rs:1155-1156`); other cooldowns for network players: *not verified* | Needs a field of its own if wanted |
| Experience level, progress, total, seed | guest | Yes (`playerdata.rs:161-164`) | |
| Game mode | guest | Yes (`playerdata.rs:166-167`) | |
| Previous game mode | single player's file only | **No** for a network player. The single player keeps it: `Level` reads it and writes it back (`save.rs:3912`, `:4198-4199` at `549d6b9cd`). A guest has no field for it: `store` never sets it, and `player_tag` drops the key unless it is set (`save.rs:3659`, `:3834-3836`), so it is lost at the guest's first save | Small: only F3+F4 and the respawn packet read it |
| Abilities | guest, compound | Flying is carried (`playerdata.rs:168`). May fly, instant build and invulnerable follow the game mode (`save.rs:3688-3706`). Fly and walk speed are kept from the loaded compound (`save.rs:3707-3714`) | |
| Respawn point, last death location | guest | Yes (`playerdata.rs:170-171`, `:175`) | |
| Recipe book | guest | Yes (`playerdata.rs:207`) | |
| Advancements | guest | Only as a file (`game/lan.rs:1433-1458`), read at join (`:1462-1490`) | Must go in the record |
| Statistics | play state | Only as a file (`game/lan.rs:1454-1457`) | Do not move (DESIGN.md) |
| Vehicle | world | By UUID only (`RootVehicle`, `playerdata.rs:177-178`, `save.rs:3857-3868`); at load the server waits for that entity to be in the world (`game/lan.rs:1389-1422`) | Refused in the first version (DESIGN.md) |
| Shoulder parrots, warden tracker, raid omen and the other `ServerPlayer` keys | compound | Kept as they were loaded, through the compound passed along (`playerdata.rs:87`, `:176`). Whether Horizon updates them for network players: *not verified* | Carried if the record holds the compound |
| Sleeping | guest (`guest.rs:126`) | No | Refuse a switch while asleep |
| Permission level | ops list of each server (`lan/hub.rs:843-855`) | Not part of the player | Each server decides; nothing to move |
| Chat session and message chain | guest (`guest.rs:75-77`) | No | Handled by the proxy (DESIGN.md section 7) |

Vanilla's own list, for comparison (javap of `addAdditionalSaveData`):
`Player` saves `Inventory`, `SelectedItemSlot`, `SleepTimer`, `XpP`, `XpLevel`,
`XpTotal`, `XpSeed`, `Score`, `abilities` and `EnderItems`; `ServerPlayer` adds
`warden_spawn_tracker`, `seenCredits`, `entered_nether_pos`,
`last_explosion_impact_pos`, `recipeBook`, `Dimension`, `respawn`,
`spawn_extra_particles_on_fall`, `raid_omen_position`, `post_effects`,
`ShoulderEntityLeft` and `ShoulderEntityRight`.

Four gaps are bugs of the save path as much as gaps for Meridian: velocity, food
exhaustion and tick timer, freezing ticks and the previous game mode. A player
who logs out and back in on the same server loses them too. They are worth fixing
in Horizon whatever Meridian does.

## 4. What the client gets at join, and what can change later

A switch must never send a new Login (DESIGN.md section 2). So whatever the
target server puts in its Login packet has to be equal to what the client already
has, or be changeable by a later packet. Horizon's Login is built in
`place_new_player` (`lan/hub.rs:874-888`) with `spawn_info` (`lan/hub.rs:798-819`).

| Login field | Horizon sends | Can change later with | For a switch |
| --- | --- | --- | --- |
| Player entity id | A new id from the server's counter (`lan/hub.rs:824-825`) | Nothing | The proxy maps it (DESIGN.md section 7) |
| Hardcore | `config.hardcore` | Nothing | Must match |
| Level keys | The three vanilla levels, always (`lan/hub.rs:858-862`) | Nothing | Always equal |
| Max players | `config.max_players` | Nothing | Must match, or the old value stays |
| Chunk radius | `chunk_radius()` | `SetChunkCacheRadius` (`horizon-protocol/src/packets/play.rs:1987`) | |
| Simulation distance | `config.simulation_distance` | `SetSimulationDistance` (`horizon-protocol/src/packets/play.rs:2006`) | |
| Reduced debug info | Always false (`lan/hub.rs:881`) | An entity event (*not verified* in Horizon) | Always equal |
| Show death screen | `!immediate_respawn` | `GameEvent` 11, immediate respawn (`lan/hub.rs:49`, sent at `:565`) | |
| Limited crafting | Always false (`lan/hub.rs:883`) | `GameEvent` 12 (javap, `ClientboundGameEventPacket`) | Always equal |
| Dimension type, dimension, hashed seed, debug, flat, sea level | From the level (`lan/hub.rs:801-807`, `:817`) | Only a Respawn | Must match: same map, same dimension |
| Game mode | The guest's | `GameEvent` 3 (`lan/hub.rs:45`) | |
| Previous game mode | Always none (`lan/hub.rs:805`) | Only a Respawn | Ignored |
| Last death location | The guest's (`lan/hub.rs:809-815`) | Only a Respawn | The client keeps the old one; same player, so the same value |
| Portal cooldown | Given by the caller | Only a Respawn | Small |
| Online mode, enforces secure chat | `lan/hub.rs:886-887` | Nothing | Must match on every backend |

After Login, Horizon sends the rest in the order of vanilla's
`PlayerList.placeNewPlayer` (javap): difficulty, abilities, held slot, recipes,
permission level and commands, recipe book, the join message to the others, a
teleport, server data, the player list, level info, the chunk cache centre,
effects, post effects and the inventory (`lan/hub.rs:889-960`). Level info
(`send_level_info`, `lan/hub.rs:1217-1249`) is the border, the time, the default
spawn, the weather, `LEVEL_CHUNKS_LOAD_START` and the tick rate.

For Meridian this has three consequences:

- `LEVEL_CHUNKS_LOAD_START` (`lan/hub.rs:1238-1243`) is what makes the client
  wait for terrain behind its loading screen. The proxy must never forward the
  target's.
- The join message and the player list update go to every player on the target
  server (`lan/hub.rs:908-917`, `:946`) at the moment the player is placed, that
  is during *prepare* in DESIGN.md's section 5, before the switch has happened.
  A held player must not be announced.
- Everything after Login is something the ghost already knows how to diff, so
  the target view can be built from it as DESIGN.md describes.

## 5. What is missing

1. **An export on demand.** Nothing serialises one connected player while they
   stay connected. The pieces exist (`store`, `player_tag`, the advancements
   JSON), but they only run when a player leaves or the world saves, and they
   miss the live values listed in section 3.
2. **An import into a live player.** A join reads its state from a file, or, for
   an integrated server's owner, from memory (section 2.2). There is no way to
   hand a joining player a record that came from elsewhere.
3. **A held player.** `place_new_player` announces the player to everyone and
   adds them to the player list the moment they are placed (`lan/hub.rs:908-918`,
   `:946`). There is no state in which a player is connected and receiving the
   world but not simulated, not shown, not announced and not hurt.
4. **A quiet release.** Removing a player always tells the others that they left
   (`lan/hub.rs:1519-1528`). Whether the other players on the old server should
   see that is a product choice, but the server should let the proxy choose.
5. **A channel the proxy can reach.** Both control channels listen on
   `127.0.0.1` only (section 2.4).
6. ~~A login that trusts the proxy.~~ It exists since `5b2537191` (section 2.6).
7. **The save gaps of section 3**: velocity, food exhaustion and tick timer,
   freezing ticks, previous game mode, and the cursor stack.

## 6. A minimal server API

### 6.1 Where it goes

The dedicated server is the game binary, so the work is in the `horizon-client` crate
(`lan/` and `game/`), not in the `horizon-server` crate, which only
supervises it. New server settings can be read the way Horizon already reads its
own keys from `server.properties` (`props.raw`, as for `max_view_distance` at
`game/dedicated.rs:851-856`).

The requests are new lines on the game's existing control endpoint, next to
`server-status` and `console` (`game/dedicated/status.rs:42-56`). They run on
the server thread between ticks, so an export sees one consistent tick and an
import lands before the next one.

The proxy does not talk to that endpoint directly, since it is local. The
recommendation is a small sidecar process next to each backend: it reads
`.horizon/engine.json`, connects to the endpoint locally, and offers the proxy
the same requests over TLS with the shared secret DESIGN.md asks for. Horizon
needs no network change for that. The alternative is to let `horizon-server`'s
HTTP API listen on a configured address with TLS and forward the new requests,
which is more work in Horizon and puts a remote surface in the server.

### 6.2 Requests

Each is one line; each answer is one JSON line, `{"ok": true, ...}` or
`{"error": "..."}`.

| Request | On | Does |
| --- | --- | --- |
| `player-expect <uuid> [seconds]` | target | The next login of `<uuid>` joins as a held player. The expectation lapses after `seconds` (the server's default, 10, when left out; 60 at most). It names no hand-over: there is none yet, since it is sent before the export. No ticket is needed: only the proxy can log a player in, through forwarding (section 6.4) |
| `player-export <uuid> freeze` | source | Closes the player's menu as vanilla does (cursor stack and crafting grid back into the inventory), stops acting on the player's own inputs, and answers with the record (6.3), which carries a `handover` id. The player stays connected and frozen. Asked again, it gives the same record, byte for byte, and does not restart the freeze |
| `player-export <uuid>` | source | The same read without a freeze: `handover` is null and the player goes on. The proxy does not use it for a switch |
| `player-import <uuid> <json>` | target | The JSON is `{"record": ..., "parts": [...], "announce": bool, "place": bool}`. Checks the record's version, data version and `handover` id (a record without one, or another player's, is refused), applies it to the held player (body included) and sends that player the resulting changes. With `"place": false` the player stays held; otherwise it is placed at once. The reply comes only once the player's file is on the target's disk |
| `player-confirm <uuid> <handover>` | target | Places a player imported with `place` false: files written first, then simulated, shown, announced |
| `player-release <uuid> <handover> moved [announce]` | source | Removes the frozen player without saving their file and, by default, without a "left the game" message. Refused when the hand-over is over (the freeze ran out, the player died, or it is another hand-over's id): the player has played on there since the export, and the record the target has is older, so the proxy fails the switch |
| `player-release <uuid> <handover> abort` | source | Unfreezes the player; nothing happened |
| `player-drop <uuid> [handover]` | target | Removes a held player whose switch was abandoned, without saving anything. After an import it names that record's `handover`, so a late drop leaves a newer arrival alone; before any import there is no id to name |

The requests are one built-in module of the server, off by default: with
`[transfer] enabled = false` every `player-*` request is answered
`{"error": "transfers are off on this server"}`. Turning it on makes the server
refuse to start without Velocity forwarding, which is what makes the proxy the
only way in. A request line is 16 MiB at most, line break included; the proxy
does not send a longer one.

This follows the phases of DESIGN.md section 5: *prepare* is `player-expect`
and the held login, *freeze* and *hand over* are `player-export freeze` then
`player-import` (and `player-confirm`), *commit* is `player-release moved`,
and a failure is `player-release abort` with `player-drop`. The hand-over id
from the export goes in every request after it: in the record the import
carries, then in the confirm, the release and the drop that follows an import.

The commit comes after the reply of the import or the confirm, so the
target's file is on disk before the source lets the player go. If the source
refuses `moved`, the switch fails: the client is disconnected, the source keeps
the player, and the copy the target placed stays in the target's player file
(older than the source's).

### 6.3 The record

A JSON object:

```json
{
  "version": 1,
  "data_version": 0,
  "handover": "<32 hexadecimal digits>",
  "uuid": "...",
  "name": "...",
  "dimension": "minecraft:overworld",
  "position": [0.0, 64.0, 0.0],
  "rotation": [0.0, 0.0],
  "velocity": [0.0, 0.0, 0.0],
  "game_mode": 0,
  "player": "<base64 of the uncompressed player compound>",
  "advancements": { },
  "live": {
    "food_exhaustion": 0.0,
    "food_tick_timer": 0,
    "ticks_frozen": 0,
    "previous_game_mode": null,
    "item_cooldowns": []
  }
}
```

- `player` is what `player_tag` builds (section 2.1). Both backends run the same
  Horizon version, so the compound is the most complete form there is, and the
  code to write and read it exists and is tested. The proxy never decodes it.
- `handover` is the id of the hand-over that an export with `freeze` starts
  (`null` for one without). The proxy reads it and sends it back in the requests
  that end the hand-over; the target refuses a record without it, and a second
  import of the same record is taken as a retry. The proxy never changes it.
- The other fields above `player` are the ones the proxy reads for itself: it
  checks the dimension and the position against the target before it imports,
  and logs them. The target reads only the `player` compound and the
  `handover` id: changing these fields changes nothing there.
- Where the player arrives is given in the import request itself, not in the
  record. With `switch-carry-position = true` the proxy sends `position`,
  `rotation` and `on_ground` beside the record (Horizon's `docs/server.md`,
  "Where the player arrives", from fork commit 46bfe5de0), with the `position`
  part; they take the place of the compound's. A server without that change,
  which is separate from the one that takes the import, ignores them and puts the
  player at the compound's place. They are where the player's client last said
  it stands and how it looks (its moves, and its answers to teleports). The client keeps moving while the
  source holds the player frozen; the export's place would pull the player back
  by what they did meanwhile. Only the import of a carried position can move the
  player on the target, so only then does the hand-over wait for the chunks
  around the new place, when the import's `recentered` says the chunk changed
  (or, without it, the view's centre moves after the import).
- `live` holds what the compound loses today (section 3). Once Horizon's save
  path carries them, they move into the compound and `live` empties.
- `advancements` is the JSON that `save_guest_records` writes.
- `data_version` is the world data version; an import with another one is
  refused.
- Effect durations are already the ticks left. Nothing in the record is an
  absolute time.

This keeps DESIGN.md's rule in spirit (a small, versioned record the proxy owns)
while not writing a second serialisation of a player in Horizon. The numbers in
the example are placeholders.

### 6.4 Login from the proxy

Nothing new in Horizon: the backends use the Velocity forwarding they already
have (section 2.6). Each backend gets the proxy's secret in `forwarding.secret`
and runs with `online-mode=false`; the proxy has already authenticated the
player. Meridian's side is to answer the `velocity:player_info` request when it
logs a player in to a backend, with version 1 of the data, signed with the same
secret.

A switch needs no extra proof in the login. The target only has to know that the
next login of this UUID is a switch, which `player-expect <uuid>` tells it on the
control channel (section 6.2); the earlier draft's ticket is no longer needed.

What is left to check is the skin: the first Meridian login that forwards
non-empty properties is also the first test of that encoding against Horizon.

### 6.5 A stopgap that needs no Horizon change

For a first test of the switch, the source can be read with
`console data get entity <name>` (section 2.5), and the target set up with
ordinary commands (`tp`, `item replace`, `effect`, `xp`, `gamemode`). It is slow,
lossy and announces the player like any join, so it can show the idea but not a
seamless switch. It is not a basis for J3.

## 7. Estimate of the work in Horizon

For someone who already knows the code, with tests:

| Piece | Days |
| --- | --- |
| Save gaps of section 3 (velocity, exhaustion and tick timer, freezing ticks, previous game mode, cursor stack on leave), each with a test | 1 |
| `player-export`: close the menu, freeze, build the record | 1 to 2 |
| Held player: the state in the hub (not announced, not tracked by the others, not simulated, not hurt) and `player-expect` | 4 to 6 |
| `player-import` into the held player, including the body, then unhold | 2 to 3 |
| `player-release` and `player-drop` | 1 |
| Two-server test of the whole exchange | 2 to 3 |
| **Total** | **11 to 16** |

The signed login from the proxy, 2 to 3 days in the first version of this
estimate, is gone: Horizon has it (section 2.6).

The held player is the largest and the riskiest piece: it has to be skipped by
every place that iterates over the guests to show, track, tick or hurt them, and
there are many (the player list, entity tracking, the tick, explosions, mob
targeting). Which of them already filter on `placed` is *not verified*.

The save gaps are worth doing first and on their own: they are bugs for any
player who logs out and back in, and they make the record smaller.

## 8. Open questions

- **Whose file is the truth.** After a switch the source server has a stale
  file for the player, and the target a fresh one. If the player later connects
  to the source without the proxy, they get the stale state. Either the proxy is
  the only way in (DESIGN.md says backends are private), or the release also
  deletes or marks the source's file.
- **A disconnect during a switch.** If the client drops between export and
  import, the record exists only in the proxy. It should be imported into the
  target as a normal save, or written back to the source with
  `player-release abort`.
- **Dimensions.** A dedicated server can place a player in a level that is not
  the one in memory (`game/lan.rs:2009-2016`). A switch between the same
  dimension on two servers is in scope; between dimensions is not (DESIGN.md
  section 1).
- **Statistics and advancements across servers.** Statistics stay per server.
  Advancements move with the record, but two servers whose advancement trees
  differ drop unknown entries at load (`game/lan.rs:1479-1482`).
- **Upstream.** Every change here belongs to Horizon and has to be agreed there
  before it is written (DESIGN.md section 12).