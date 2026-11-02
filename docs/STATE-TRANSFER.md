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

Paths below are relative to `crates/` in the Horizon repository.

## 1. How a Horizon server holds a player

A Horizon dedicated server is the game itself running with no local player.
`horizon-server` starts the game binary with `--dedicated --nogui` and a control
endpoint (`horizon-server/src/engine.rs:233-260`). The game then opens its world
and serves the network through the same code that opens a single player world to
the LAN (`horizon-client/src/game/dedicated.rs:842`, `lan_host`). Every connected
player is a *guest* of that LAN host. The dedicated server explicitly drops the
single player's own state, because players have their own files
(`horizon-client/src/game/dedicated.rs:713-714`).

So the state of a connected player lives in three places:

- **The guest** (`horizon-client/src/lan/guest.rs:63-306`, `struct Guest`):
  position, rotation, game mode, inventory, armour, off hand, cursor stack,
  ender chest, health, food, saturation, experience, effects, the open menu,
  the respawn point, the advancements in memory, the recipe book, and the
  untouched player compound read from disk (`saved`, line 285).
- **The guest's body** (`motion: Option<Box<PlayerMotion>>`, `guest.rs:195`):
  the simulated player, with velocity, fire ticks, air, fall distance, absorption
  and food exhaustion. The `lan status` request reads air, absorption and
  exhaustion from it (`horizon-client/src/game/lan.rs:1166-1173`).
- **The host's per-connection play state** (`host.play`, keyed by connection):
  the statistics (`game/lan.rs:1454-1457`), the shield cooldown and the item in
  use (`game/lan.rs:1155-1159`), and the open crafting table or workstation
  (`game/lan_crafting.rs:640-647`).

## 2. What already exists

### 2.1 A complete save and load of the player compound

`horizon-client/src/lan/playerdata.rs` saves and loads a guest in the vanilla
player compound, through the same code that saves the single player:

- `store` (`playerdata.rs:148-229`) copies a live guest into the player fields
  of a `Level`. `save` (`:134-145`) then writes `players/data/<uuid>.dat`.
- `load` (`:42-56`) reads that file and `apply` (`:59-131`) gives its state to a
  guest.
- `Level::player_tag` (`horizon-client/src/save.rs:3645-3870`) builds the
  compound by starting from the compound that was loaded and writing Horizon's
  own fields over it, so keys Horizon does not model survive a load and a save.
  `Level::read_player` (`save.rs:3510-3643`) is the reverse.

Advancements and statistics are separate JSON files, written by
`save_guest_records` (`game/lan.rs:1433-1458`) and read by `load_guest_records`
(`game/lan.rs:1462-1490`), as vanilla's `PlayerAdvancements` and
`ServerStatsCounter` do.

This is most of an export and an import already. What is missing is a way to
run it on demand, for one player, without the player leaving, and to take the
result from memory instead of a file (section 4).

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
the answer one JSON line (`horizon-client/src/control.rs:1-11`, `serve` at
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
forwards to `console` above (`api.rs:512-527`).

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
There is no mode in which the server trusts an identity given by a proxy: no
forwarding of the real UUID and profile properties, and nothing in `NetConfig`
for it (`lan/mod.rs:147-189`). A server behind a proxy in offline mode would
give each player an offline UUID instead of their account's and lose their
skin.
</content>
</invoke>

## 3. Field by field

What section 6 of DESIGN.md asks for, against what the save path carries today.
"Carried" means `store` and `player_tag` write it and `read_player` and `apply`
read it back, so a save on one server and a load on another keep it.
`playerdata.rs` is `horizon-client/src/lan/playerdata.rs`.

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
| Freezing ticks | body | **No.** `SavedBody` has no field for them (`guest.rs:27-31`) | Small |
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
| Previous game mode | none | **No.** `store` never sets it, and `player_tag` drops the key unless it is set (`save.rs:3659`, `:3834-3836`) | Small: only F3+F4 and the respawn packet read it |
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
| Simulation distance | `config.simulation_distance` | `SetSimulationDistance` (`play.rs:2006`) | |
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