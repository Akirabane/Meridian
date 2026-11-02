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
