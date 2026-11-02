# Packet inventory for the transfer

This document lists every packet of the Configuration and Play states for Minecraft Java 26.3 (protocol 777). It sorts each packet by what Meridian has to do with it when a player moves from one server to another server of the same map without seeing it.

The [switch checklist](#switch-checklist) at the end sums up what the proxy must do.

## Sources

- **Packet ids and names:** `crates/horizon-protocol/src/ids.rs` in Horizon. That file is generated from the vanilla data generator's packet report, so it lists every packet of 26.3 with its exact id.
- **Horizon support:** the `Clientbound` and `Serverbound` enums in `crates/horizon-protocol/src/packets/configuration.rs` and `packets/play.rs`. In the tables, "typed" means Horizon decodes the packet into a struct. "raw" means Horizon keeps it as `Unknown(id, bytes)`, which is enough for a packet the proxy only relays.
- **Fields:** read from the vanilla 26.3 client classes. The class column gives the class under `net.minecraft.network.protocol.`, so `game.ClientboundLoginPacket` is `net.minecraft.network.protocol.game.ClientboundLoginPacket`. The fields column lists the instance fields in declaration order. That order usually, but not always, matches the wire order.
- **Behaviour:** a note that names a method, such as `ClientPacketListener.handleRespawn`, was checked in that method of the client. A note that says "not verified" was not checked. Anything else in a note follows from the fields alone.

This document contains no game code or game files, only packet names, class names and field names.

## Categories

The categories follow [DESIGN.md](DESIGN.md): the ghost (section 4), switching (section 5) and rewriting (section 7). A packet can belong to more than one category.

| Cat. | Meaning | What the proxy does |
| --- | --- | --- |
| 1 | Carries an entity id: the player's own id or another entity's id. | Rewrites the id in both directions with the per-backend map of DESIGN.md section 7. The player's own id is the one from the client's first Login. |
| 2 | Carries world or player state that the client keeps. | Updates the ghost from every forwarded packet. During a switch, reads the target's packets into the target view instead, and sends only the delta (DESIGN.md section 4). |
| 3 | Tied to one connection or session. | Keeps its own numbering on the client side and maps it per backend: keep-alive and ping, teleport ids, block-change sequences, container and state ids, transaction ids, chunk batches, signed chat (DESIGN.md section 7). |
| 4 | Neutral. | Relays the packet as it is, as bytes. |
| 5 | Forces a loading screen or a visible change, or ends the connection. | Never lets it reach the client after the first login (DESIGN.md sections 2 and 5). |
