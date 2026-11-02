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

## Configuration, clientbound

| Id | Packet | Vanilla class | Fields | Horizon | Cat. | Notes |
| --- | --- | --- | --- | --- | --- | --- |
| 0 | `cookie_request` | `cookie.ClientboundCookieRequestPacket` | `key` | typed | 4 | Asks the client for a cookie it stored earlier. |
| 1 | `custom_payload` | `common.ClientboundCustomPayloadPacket` | `payload` | typed | 4 | Plugin channel; content depends on the channel. |
| 2 | `disconnect` | `common.ClientboundDisconnectPacket` | `reason` | typed | 5 | Ends the connection. |
| 3 | `finish_configuration` | `configuration.ClientboundFinishConfigurationPacket` | none | typed | 5 | Ends configuration; the client then waits for Login in Play. |
| 4 | `keep_alive` | `common.ClientboundKeepAlivePacket` | `id` | typed | 3 | The client echoes `id`. |
| 5 | `ping` | `common.ClientboundPingPacket` | `id` | typed | 3 | The client echoes `id` in Pong. |
| 6 | `reset_chat` | `configuration.ClientboundResetChatPacket` | none | typed | 3 | Resets the chat signing chain. |
| 7 | `registry_data` | `configuration.ClientboundRegistryDataPacket` | `registry`, `entries` | typed | 2 | Synced registries. Both servers must send the same data, or the client needs a new configuration. |
| 8 | `resource_pack_pop` | `common.ClientboundResourcePackPopPacket` | `id` | typed | 2 |  |
| 9 | `resource_pack_push` | `common.ClientboundResourcePackPushPacket` | `id`, `url`, `hash`, `required`, `prompt` | typed | 2 |  |
| 10 | `post_effects` | `common.ClientboundPostEffectsPacket` | `postEffects` | raw | 2 |  |
| 11 | `store_cookie` | `common.ClientboundStoreCookiePacket` | `key`, `payload` | typed | 4 |  |
| 12 | `transfer` | `common.ClientboundTransferPacket` | `host`, `port` | typed | 5 | Disconnects and reconnects to another address. |
| 13 | `update_enabled_features` | `configuration.ClientboundUpdateEnabledFeaturesPacket` | `features` | typed | 2 | Must match between servers, like the registries. |
| 14 | `update_tags` | `common.ClientboundUpdateTagsPacket` | `tags` | typed | 2 | Must match between servers, like the registries. |
| 15 | `select_known_packs` | `configuration.ClientboundSelectKnownPacks` | `knownPacks` | typed | 2 |  |
| 16 | `custom_report_details` | `common.ClientboundCustomReportDetailsPacket` | `details` | raw | 4 |  |
| 17 | `server_links` | `common.ClientboundServerLinksPacket` | `links` | typed | 2 |  |
| 18 | `clear_dialog` | `common.ClientboundClearDialogPacket` | none | typed | 4 |  |
| 19 | `show_dialog` | `common.ClientboundShowDialogPacket` | `dialog` | typed | 4 |  |
| 20 | `code_of_conduct` | `configuration.ClientboundCodeOfConductPacket` | `codeOfConduct` | typed | 4 |  |

## Configuration, serverbound

| Id | Packet | Vanilla class | Fields | Horizon | Cat. | Notes |
| --- | --- | --- | --- | --- | --- | --- |
| 0 | `client_information` | `common.ServerboundClientInformationPacket` | `information` | typed | 2 | Language, view distance, skin parts. The proxy keeps the last one to give to the next server. |
| 1 | `cookie_response` | `cookie.ServerboundCookieResponsePacket` | `key`, `payload` | typed | 4 |  |
| 2 | `custom_payload` | `common.ServerboundCustomPayloadPacket` | `payload` | typed | 4 |  |
| 3 | `finish_configuration` | `configuration.ServerboundFinishConfigurationPacket` | none | typed | 5 | Answer to Finish Configuration. |
| 4 | `keep_alive` | `common.ServerboundKeepAlivePacket` | `id` | typed | 3 |  |
| 5 | `pong` | `common.ServerboundPongPacket` | `id` | typed | 3 |  |
| 6 | `resource_pack` | `common.ServerboundResourcePackPacket` | `id`, `action` | typed | 3 | Pack status, keyed by the pack UUID the server sent. |
| 7 | `select_known_packs` | `configuration.ServerboundSelectKnownPacks` | `knownPacks` | typed | 2 |  |
| 8 | `custom_click_action` | `common.ServerboundCustomClickActionPacket` | `id`, `payload` | typed | 4 |  |
| 9 | `accept_code_of_conduct` | `configuration.ServerboundAcceptCodeOfConductPacket` | none | typed | 4 |  |

## Play, clientbound

| Id | Packet | Vanilla class | Fields | Horizon | Cat. | Notes |
| --- | --- | --- | --- | --- | --- | --- |
| 0 | `bundle_delimiter` | `game.ClientboundBundleDelimiterPacket` | none | typed | 3 | Opens and closes a bundle. The client applies the packets between two delimiters in one go, so the proxy must never cut a bundle or mix in packets from another server. |
| 1 | `add_entity` | `game.ClientboundAddEntityPacket` | `id`, `uuid`, `type`, `x`, `y`, `z`, `movement`, `xRot`, `yRot`, `yHeadRot`, `data` | typed | 1 | `id` is the new entity. For projectiles `data` is the owner's entity id, or 0 (`Projectile.getAddEntityPacket`). |
| 2 | `animate` | `game.ClientboundAnimatePacket` | `id`, `action` | typed | 1 |  |
| 3 | `award_stats` | `game.ClientboundAwardStatsPacket` | `stats` | typed | 4 |  |
| 4 | `block_changed_ack` | `game.ClientboundBlockChangedAckPacket` | `sequence` | typed | 3 | `sequence` answers a serverbound `sequence` (Player Action, Use Item, Use Item On). |
| 5 | `block_destruction` | `game.ClientboundBlockDestructionPacket` | `id`, `pos`, `progress` | typed | 1 | `id` is the entity breaking the block. |
| 6 | `block_entity_data` | `game.ClientboundBlockEntityDataPacket` | `pos`, `type`, `tag` | typed | 4 | Changes inside loaded chunks; chunk data already covers them at a switch. |
| 7 | `block_event` | `game.ClientboundBlockEventPacket` | `pos`, `b0`, `b1`, `block` | typed | 4 |  |
| 8 | `block_update` | `game.ClientboundBlockUpdatePacket` | `pos`, `blockState` | typed | 4 | Changes inside loaded chunks; chunk data already covers them at a switch. |
| 9 | `boss_event` | `game.ClientboundBossEventPacket` | `id`, `operation` | typed | 2 | Bars are keyed by UUID. Bars from the old server must be removed. |
| 10 | `change_difficulty` | `game.ClientboundChangeDifficultyPacket` | `difficulty`, `locked` | typed | 2 |  |
| 11 | `chunk_batch_finished` | `game.ClientboundChunkBatchFinishedPacket` | `batchSize` | typed | 2,3 | Closes a chunk batch; the client answers with Chunk Batch Received. |
| 12 | `chunk_batch_start` | `game.ClientboundChunkBatchStartPacket` | none | typed | 2,3 |  |
| 13 | `chunks_biomes` | `game.ClientboundChunksBiomesPacket` | `chunkBiomeData` | typed | 2 |  |
| 14 | `clear_titles` | `game.ClientboundClearTitlesPacket` | `resetTimes` | typed | 4 |  |
| 15 | `command_suggestions` | `game.ClientboundCommandSuggestionsPacket` | `id`, `start`, `length`, `suggestions` | raw | 3 | `id` answers a serverbound Command Suggestion. |
| 16 | `commands` | `game.ClientboundCommandsPacket` | `rootIndex`, `entries` | typed | 2 | The full command tree; each server sends its own. |
| 17 | `container_close` | `game.ClientboundContainerClosePacket` | `containerId` | typed | 3 |  |
| 18 | `container_set_content` | `game.ClientboundContainerSetContentPacket` | `containerId`, `stateId`, `items`, `carriedItem` | typed | 2,3 | `containerId` and `stateId` belong to one server. Container 0 is the player inventory. |
| 19 | `container_set_data` | `game.ClientboundContainerSetDataPacket` | `containerId`, `id`, `value` | typed | 3 |  |
| 20 | `container_set_slot` | `game.ClientboundContainerSetSlotPacket` | `containerId`, `stateId`, `slot`, `itemStack` | typed | 2,3 | Same ids as Set Content. |
| 21 | `cookie_request` | `cookie.ClientboundCookieRequestPacket` | `key` | typed | 4 |  |
| 22 | `cooldown` | `game.ClientboundCooldownPacket` | `cooldownGroup`, `duration` | typed | 2 | Item cooldowns of the player. |
| 23 | `custom_chat_completions` | `game.ClientboundCustomChatCompletionsPacket` | `action`, `entries` | typed | 2 |  |
| 24 | `custom_payload` | `common.ClientboundCustomPayloadPacket` | `payload` | typed | 4 | Plugin channel; content depends on the channel. |
| 25 | `damage_event` | `game.ClientboundDamageEventPacket` | `entityId`, `sourceType`, `sourceCauseId`, `sourceDirectId`, `sourcePosition` | typed | 1 | `entityId`, `sourceCauseId` and `sourceDirectId` are entity ids. How the source ids encode "none" is not verified. |
| 26 | `debug/block_value` | `game.ClientboundDebugBlockValuePacket` | `blockPos`, `update` | raw | 4 | Debug only. |
| 27 | `debug/chunk_value` | `game.ClientboundDebugChunkValuePacket` | `chunkPos`, `update` | raw | 4 | Debug only. |
| 28 | `debug/entity_value` | `game.ClientboundDebugEntityValuePacket` | `entityId`, `update` | raw | 1 | Debug only. |
| 29 | `debug/event` | `game.ClientboundDebugEventPacket` | `event` | raw | 4 | Debug only. |
| 30 | `debug_sample` | `game.ClientboundDebugSamplePacket` | `sample`, `debugSampleType` | raw | 4 | Debug only. |
| 31 | `delete_chat` | `game.ClientboundDeleteChatPacket` | `messageSignature` | typed | 3 | Refers to a signed message by signature or index. |
| 32 | `disconnect` | `common.ClientboundDisconnectPacket` | `reason` | typed | 5 | Ends the connection; at a switch the proxy drops it and keeps the client connected. |
| 33 | `disguised_chat` | `game.ClientboundDisguisedChatPacket` | `message`, `chatType` | typed | 4 |  |
| 34 | `entity_event` | `game.ClientboundEntityEventPacket` | `entityId`, `eventId` | typed | 1 |  |
| 35 | `entity_position_sync` | `game.ClientboundEntityPositionSyncPacket` | `id`, `position`, `yRot`, `xRot`, `onGround` | typed | 1 |  |
| 36 | `explode` | `game.ClientboundExplodePacket` | `center`, `radius`, `blockCount`, `playerKnockback`, `explosionParticle`, `explosionSound`, `blockParticles`, `playSound` | typed | 4 |  |
| 37 | `add_transient_block` | `game.ClientboundAddTransientBlockPacket` | `pos`, `blockState` | typed | 4 |  |
| 38 | `forget_level_chunk` | `game.ClientboundForgetLevelChunkPacket` | `pos` | typed | 2 | Unloads a chunk on the client. |
| 39 | `game_event` | `game.ClientboundGameEventPacket` | `event`, `param` | typed | 2,5 | Weather, game mode, rules. Event 13 `LEVEL_CHUNKS_LOAD_START` tells the client's level load tracker that loading packets arrived (`ClientPacketListener.handleGameEvent`); it belongs to a Login or Respawn and must not pass at a switch. Event 4 `WIN_GAME` opens the credits screen. |
| 40 | `game_rule_values` | `game.ClientboundGameRuleValuesPacket` | `values` | typed | 2 |  |
| 41 | `game_test_highlight_pos` | `game.ClientboundGameTestHighlightPosPacket` | `absolutePos`, `relativePos` | typed | 4 |  |
| 42 | `mount_screen_open` | `game.ClientboundMountScreenOpenPacket` | `containerId`, `inventoryColumns`, `entityId` | typed | 1,3 | `entityId` is the mount; `containerId` belongs to the server. |
| 43 | `hurt_animation` | `game.ClientboundHurtAnimationPacket` | `id`, `yaw` | typed | 1 |  |
| 44 | `initialize_border` | `game.ClientboundInitializeBorderPacket` | `newCenterX`, `newCenterZ`, `oldSize`, `newSize`, `lerpTime`, `newAbsoluteMaxSize`, `warningBlocks`, `warningTime` | typed | 2 |  |
| 45 | `keep_alive` | `common.ClientboundKeepAlivePacket` | `id` | typed | 3 | The client echoes `id`; each server checks its own ids. |
| 46 | `level_chunk_with_light` | `game.ClientboundLevelChunkWithLightPacket` | `x`, `z`, `chunkData`, `lightData` | typed | 2 |  |
| 47 | `level_event` | `game.ClientboundLevelEventPacket` | `type`, `pos`, `data`, `globalEvent` | typed | 4 |  |
| 48 | `level_particles` | `game.ClientboundLevelParticlesPacket` | `particle`, `overrideLimiter`, `alwaysShow`, `x`, `y`, `z`, `xDist`, `yDist`, `zDist`, `xMaxSpeed`, `yMaxSpeed`, `zMaxSpeed`, `count`, `randomizationType` | typed | 4 |  |
| 49 | `light_update` | `game.ClientboundLightUpdatePacket` | `x`, `z`, `lightData` | typed | 2 |  |
| 50 | `login` | `game.ClientboundLoginPacket` | `playerId`, `hardcore`, `levels`, `maxPlayers`, `chunkRadius`, `simulationDistance`, `reducedDebugInfo`, `showDeathScreen`, `doLimitedCrafting`, `commonPlayerSpawnInfo`, `onlineMode`, `enforcesSecureChat` | typed | 1,2,5 | `playerId` is the id the client gives itself. The client builds a new world and starts waiting for it to load (`ClientPacketListener.handleLogin` calls `startWaitingForNewLevel`). Only the first one may reach the client. |
| 51 | `low_disk_space_warning` | `game.ClientboundLowDiskSpaceWarningPacket` | none | typed | 4 |  |
| 52 | `map_item_data` | `game.ClientboundMapItemDataPacket` | `mapId`, `scale`, `locked`, `decorations`, `colorPatch` | typed | 2 | Map ids are per world; the two servers must share them. |
| 53 | `merchant_offers` | `game.ClientboundMerchantOffersPacket` | `containerId`, `offers`, `villagerLevel`, `villagerXp`, `showProgress`, `canRestock` | typed | 3 |  |
| 54 | `move_entity_pos` | `game.ClientboundMoveEntityPacket.Pos` | `entityId`, `delta`, `onGround` | typed | 1 |  |
| 55 | `move_entity_pos_rot` | `game.ClientboundMoveEntityPacket.PosRot` | `entityId`, `delta`, `yRot`, `xRot`, `onGround` | typed | 1 |  |
| 56 | `move_minecart_along_track` | `game.ClientboundMoveMinecartPacket` | `entityId`, `lerpSteps` | typed | 1 |  |
| 57 | `move_entity_rot` | `game.ClientboundMoveEntityPacket.Rot` | `entityId`, `yRot`, `xRot`, `onGround` | typed | 1 |  |
| 58 | `move_vehicle` | `game.ClientboundMoveVehiclePacket` | `movingTo` | typed | 2 | Position of the vehicle the player drives. |
| 59 | `open_book` | `game.ClientboundOpenBookPacket` | `hand` | typed | 4 |  |
| 60 | `open_screen` | `game.ClientboundOpenScreenPacket` | `containerId`, `type`, `title` | typed | 3 | `containerId` belongs to the server. |
| 61 | `open_sign_editor` | `game.ClientboundOpenSignEditorPacket` | `pos`, `slot` | typed | 4 |  |
| 62 | `ping` | `common.ClientboundPingPacket` | `id` | typed | 3 | The client echoes `id` in Pong. |
| 63 | `pong_response` | `ping.ClientboundPongResponsePacket` | `time` | typed | 3 | Answer to the client's Ping Request. |
| 64 | `place_ghost_recipe` | `game.ClientboundPlaceGhostRecipePacket` | `containerId`, `recipeDisplay` | typed | 3 |  |
| 65 | `player_abilities` | `game.ClientboundPlayerAbilitiesPacket` | `invulnerable`, `isFlying`, `canFly`, `instabuild`, `flyingSpeed`, `walkingSpeed` | typed | 2 |  |
| 66 | `player_chat` | `game.ClientboundPlayerChatPacket` | `globalIndex`, `sender`, `index`, `signature`, `body`, `unsignedContent`, `filterMask`, `chatType` | typed | 3 | Signed chat. `index`, `globalIndex` and the signature chain belong to one server session. |
| 67 | `player_combat_end` | `game.ClientboundPlayerCombatEndPacket` | `duration` | raw | 4 |  |
| 68 | `player_combat_enter` | `game.ClientboundPlayerCombatEnterPacket` | none | raw | 4 |  |
| 69 | `player_combat_kill` | `game.ClientboundPlayerCombatKillPacket` | `playerId`, `message` | typed | 1 | `playerId` is the dead player. It opens the death screen. |
| 70 | `player_info_remove` | `game.ClientboundPlayerInfoRemovePacket` | `profileIds` | typed | 2 | Tab list, by profile UUID. |
| 71 | `player_info_update` | `game.ClientboundPlayerInfoUpdatePacket` | `actions`, `entries` | typed | 2,3 | Tab list, by profile UUID. Can carry each player's chat session key. |
| 72 | `player_look_at` | `game.ClientboundPlayerLookAtPacket` | `x`, `y`, `z`, `entity`, `fromAnchor`, `toAnchor`, `atEntity` | typed | 1 | `entity` is used when `atEntity` is set. |
| 73 | `player_position` | `game.ClientboundPlayerPositionPacket` | `id`, `change`, `relatives` | typed | 2,3 | Moves the player. `id` is a teleport id the client confirms with Accept Teleportation. |
| 74 | `player_rotation` | `game.ClientboundPlayerRotationPacket` | `yRot`, `relativeY`, `xRot`, `relativeX` | typed | 4 |  |
| 75 | `recipe_book_add` | `game.ClientboundRecipeBookAddPacket` | `entries`, `replace` | typed | 2 |  |
| 76 | `recipe_book_remove` | `game.ClientboundRecipeBookRemovePacket` | `recipes` | typed | 2 |  |
| 77 | `recipe_book_settings` | `game.ClientboundRecipeBookSettingsPacket` | `bookSettings` | typed | 2 |  |
| 78 | `remove_entities` | `game.ClientboundRemoveEntitiesPacket` | `entityIds` | typed | 1,2 | At a switch the old server's entities must be removed with this packet. |
| 79 | `remove_mob_effect` | `game.ClientboundRemoveMobEffectPacket` | `entityId`, `effect` | typed | 1,2 |  |
| 80 | `reset_score` | `game.ClientboundResetScorePacket` | `owner`, `objectiveName` | typed | 2 |  |
| 81 | `resource_pack_pop` | `common.ClientboundResourcePackPopPacket` | `id` | typed | 2 |  |
| 82 | `resource_pack_push` | `common.ClientboundResourcePackPushPacket` | `id`, `url`, `hash`, `required`, `prompt` | typed | 2 | Whether a different pack shows a reload screen is not verified here. |
| 83 | `post_effects` | `common.ClientboundPostEffectsPacket` | `postEffects` | typed | 2 |  |
| 84 | `respawn` | `game.ClientboundRespawnPacket` | `commonPlayerSpawnInfo`, `dataToKeep` | typed | 2,5 | The client builds a new world and starts waiting for it to load (`ClientPacketListener.handleRespawn`). Must never pass at a switch. |
| 85 | `rotate_head` | `game.ClientboundRotateHeadPacket` | `entityId`, `yHeadRot` | typed | 1 |  |
| 86 | `section_blocks_update` | `game.ClientboundSectionBlocksUpdatePacket` | `sectionPos`, `positions`, `states` | typed | 4 | Changes inside loaded chunks. |
| 87 | `select_advancements_tab` | `game.ClientboundSelectAdvancementsTabPacket` | `tab` | raw | 4 |  |
| 88 | `server_data` | `game.ClientboundServerDataPacket` | `motd`, `iconBytes` | typed | 2 | MOTD and icon. |
| 89 | `set_action_bar_text` | `game.ClientboundSetActionBarTextPacket` | `text` | typed | 4 |  |
| 90 | `set_border_center` | `game.ClientboundSetBorderCenterPacket` | `newCenterX`, `newCenterZ` | typed | 2 |  |
| 91 | `set_border_lerp_size` | `game.ClientboundSetBorderLerpSizePacket` | `oldSize`, `newSize`, `lerpTime` | typed | 2 |  |
| 92 | `set_border_size` | `game.ClientboundSetBorderSizePacket` | `size` | typed | 2 |  |
| 93 | `set_border_warning_delay` | `game.ClientboundSetBorderWarningDelayPacket` | `warningDelay` | typed | 2 |  |
| 94 | `set_border_warning_distance` | `game.ClientboundSetBorderWarningDistancePacket` | `warningBlocks` | typed | 2 |  |
| 95 | `set_camera` | `game.ClientboundSetCameraPacket` | `cameraId` | typed | 1 |  |
| 96 | `set_chunk_cache_center` | `game.ClientboundSetChunkCacheCenterPacket` | `x`, `z` | typed | 2 |  |
| 97 | `set_chunk_cache_radius` | `game.ClientboundSetChunkCacheRadiusPacket` | `radius` | typed | 2 |  |
| 98 | `set_cursor_item` | `game.ClientboundSetCursorItemPacket` | `contents` | typed | 2 |  |
| 99 | `set_default_spawn_position` | `game.ClientboundSetDefaultSpawnPositionPacket` | `respawnData` | typed | 2 |  |
| 100 | `set_display_objective` | `game.ClientboundSetDisplayObjectivePacket` | `slot`, `objectiveName` | typed | 2 |  |
| 101 | `set_entity_data` | `game.ClientboundSetEntityDataPacket` | `id`, `packedItems` | typed | 1 | Some metadata values are entity ids too: `FishingHook.DATA_HOOKED_ENTITY`, `Guardian.DATA_ID_ATTACK_TARGET`, `WitherBoss.DATA_TARGET_A/B/C` (int) and `FireworkRocketEntity.DATA_ATTACHED_TO_TARGET` (optional int). Whether the int values are offset by one is not verified. |
| 102 | `set_entity_link` | `game.ClientboundSetEntityLinkPacket` | `sourceId`, `destId` | typed | 1 | Leash: both fields are entity ids. |
| 103 | `set_entity_motion` | `game.ClientboundSetEntityMotionPacket` | `id`, `movement` | typed | 1 |  |
| 104 | `set_equipment` | `game.ClientboundSetEquipmentPacket` | `entity`, `slots` | typed | 1 |  |
| 105 | `set_experience` | `game.ClientboundSetExperiencePacket` | `experienceProgress`, `totalExperience`, `experienceLevel` | typed | 2 |  |
| 106 | `set_health` | `game.ClientboundSetHealthPacket` | `health`, `food`, `saturation` | typed | 2 |  |
| 107 | `set_held_slot` | `game.ClientboundSetHeldSlotPacket` | `slot` | typed | 2 |  |
| 108 | `set_objective` | `game.ClientboundSetObjectivePacket` | `objectiveName`, `displayName`, `renderType`, `numberFormat`, `method` | typed | 2 |  |
| 109 | `set_passengers` | `game.ClientboundSetPassengersPacket` | `vehicle`, `passengers` | typed | 1 | `vehicle` and every passenger are entity ids. |
| 110 | `set_player_inventory` | `game.ClientboundSetPlayerInventoryPacket` | `slot`, `contents` | typed | 2 |  |
| 111 | `set_player_team` | `game.ClientboundSetPlayerTeamPacket` | `method`, `name`, `players`, `parameters` | typed | 2 | Teams list players by name, not by id. |
| 112 | `set_score` | `game.ClientboundSetScorePacket` | `owner`, `objectiveName`, `score`, `display`, `numberFormat` | typed | 2 |  |
| 113 | `set_simulation_distance` | `game.ClientboundSetSimulationDistancePacket` | `simulationDistance` | typed | 2 |  |
| 114 | `set_subtitle_text` | `game.ClientboundSetSubtitleTextPacket` | `text` | typed | 4 |  |
| 115 | `set_time` | `game.ClientboundSetTimePacket` | `gameTime`, `clockUpdates` | typed | 2 |  |
| 116 | `set_title_text` | `game.ClientboundSetTitleTextPacket` | `text` | typed | 4 |  |
| 117 | `set_titles_animation` | `game.ClientboundSetTitlesAnimationPacket` | `fadeIn`, `stay`, `fadeOut` | typed | 4 |  |
| 118 | `sound_entity` | `game.ClientboundSoundEntityPacket` | `sound`, `source`, `id`, `volume`, `pitch`, `seed` | typed | 1 |  |
| 119 | `sound` | `game.ClientboundSoundPacket` | `sound`, `source`, `x`, `y`, `z`, `volume`, `pitch`, `seed` | typed | 4 |  |
| 120 | `start_configuration` | `game.ClientboundStartConfigurationPacket` | none | typed | 5 | The client clears its world and shows the reconfiguration screen (`ClientPacketListener.handleConfigurationStart`). Must never pass at a switch. |
| 121 | `stop_sound` | `game.ClientboundStopSoundPacket` | `name`, `source` | typed | 4 |  |
| 122 | `store_cookie` | `common.ClientboundStoreCookiePacket` | `key`, `payload` | typed | 4 |  |
| 123 | `swing_animation` | `game.ClientboundSwingAnimationPacket` | `entityId`, `hand`, `animation` | typed | 1 |  |
| 124 | `system_chat` | `game.ClientboundSystemChatPacket` | `content`, `overlay` | typed | 4 |  |
| 125 | `tab_list` | `game.ClientboundTabListPacket` | `header`, `footer` | typed | 2 | Header and footer. |
| 126 | `tag_query` | `game.ClientboundTagQueryPacket` | `transactionId`, `tag` | typed | 3 | `transactionId` answers a serverbound tag query. |
| 127 | `take_item_entity` | `game.ClientboundTakeItemEntityPacket` | `itemId`, `playerId`, `amount` | typed | 1 | `itemId` and `playerId` are entity ids. |
| 128 | `teleport_entity` | `game.ClientboundTeleportEntityPacket` | `id`, `change`, `relatives`, `onGround` | typed | 1 |  |
| 129 | `test_instance_block_status` | `game.ClientboundTestInstanceBlockStatus` | `status`, `size` | typed | 4 |  |
| 130 | `ticking_state` | `game.ClientboundTickingStatePacket` | `tickRate`, `isFrozen` | typed | 2 |  |
| 131 | `ticking_step` | `game.ClientboundTickingStepPacket` | `tickSteps` | typed | 4 |  |
| 132 | `transfer` | `common.ClientboundTransferPacket` | `host`, `port` | typed | 5 | The client disconnects and opens the connect screen (`ClientCommonPacketListenerImpl.handleTransfer`). |
| 133 | `update_advancements` | `game.ClientboundUpdateAdvancementsPacket` | `shouldReset`, `added`, `removed`, `progress`, `showAdvancements` | typed | 2 | `shouldReset` clears the client's advancements first. |
| 134 | `update_attributes` | `game.ClientboundUpdateAttributesPacket` | `entityId`, `attributes` | typed | 1,2 |  |
| 135 | `update_mob_effect` | `game.ClientboundUpdateMobEffectPacket` | `entityId`, `effect`, `effectAmplifier`, `effectDurationTicks`, `flags` | typed | 1,2 |  |
| 136 | `update_recipes` | `game.ClientboundUpdateRecipesPacket` | `itemSets`, `stonecutterRecipes` | raw | 2 |  |
| 137 | `update_tags` | `common.ClientboundUpdateTagsPacket` | `tags` | typed | 2 | Must match between servers. |
| 138 | `projectile_power` | `game.ClientboundProjectilePowerPacket` | `id`, `accelerationPower` | typed | 1 |  |
| 139 | `custom_report_details` | `common.ClientboundCustomReportDetailsPacket` | `details` | raw | 4 |  |
| 140 | `server_links` | `common.ClientboundServerLinksPacket` | `links` | typed | 2 |  |
| 141 | `waypoint` | `game.ClientboundTrackedWaypointPacket` | `operation`, `waypoint` | typed | 2 | Locator bar. Waypoints are keyed by a UUID or a name. |
| 142 | `clear_dialog` | `common.ClientboundClearDialogPacket` | none | typed | 4 |  |
| 143 | `show_dialog` | `common.ClientboundShowDialogPacket` | `dialog` | typed | 4 |  |

## Play, serverbound

| Id | Packet | Vanilla class | Fields | Horizon | Cat. | Notes |
| --- | --- | --- | --- | --- | --- | --- |
| 0 | `accept_teleportation` | `game.ServerboundAcceptTeleportationPacket` | `id`, `x`, `y`, `z`, `yRot`, `xRot` | typed | 3 | Confirms the teleport `id` of a Player Position packet. |
| 1 | `attack` | `game.ServerboundAttackPacket` | `entityId` | typed | 1 |  |
| 2 | `block_entity_tag_query` | `game.ServerboundBlockEntityTagQueryPacket` | `transactionId`, `pos` | typed | 3 |  |
| 3 | `bundle_item_selected` | `game.ServerboundSelectBundleItemPacket` | `slotId`, `selectedItemIndex` | typed | 3 | `slotId` is a slot of the open container. |
| 4 | `change_difficulty` | `game.ServerboundChangeDifficultyPacket` | `difficulty` | typed | 4 |  |
| 5 | `change_game_mode` | `game.ServerboundChangeGameModePacket` | `mode` | typed | 4 |  |
| 6 | `chat_ack` | `game.ServerboundChatAckPacket` | `offset` | typed | 3 | Acknowledges signed messages by count; tied to one server session. |
| 7 | `chat_command` | `game.ServerboundChatCommandPacket` | `command` | typed | 4 |  |
| 8 | `chat_command_signed` | `game.ServerboundChatCommandSignedPacket` | `command`, `timeStamp`, `salt`, `argumentSignatures`, `lastSeenMessages` | typed | 3 | Signed; carries last-seen messages. |
| 9 | `chat` | `game.ServerboundChatPacket` | `message`, `timeStamp`, `salt`, `signature`, `lastSeenMessages` | typed | 3 | Signed; carries last-seen messages. |
| 10 | `chat_session_update` | `game.ServerboundChatSessionUpdatePacket` | `chatSession` | typed | 3 | The client's chat key. The proxy keeps it to give to the next server. |
| 11 | `chunk_batch_received` | `game.ServerboundChunkBatchReceivedPacket` | `desiredChunksPerTick` | typed | 3 | Answer to Chunk Batch Finished. |
| 12 | `client_command` | `game.ServerboundClientCommandPacket` | `action` | typed | 4 | Respawn request or stats request. |
| 13 | `client_tick_end` | `game.ServerboundClientTickEndPacket` | none | typed | 4 |  |
| 14 | `client_information` | `common.ServerboundClientInformationPacket` | `information` | typed | 2 | The proxy keeps the last one to give to the next server. |
| 15 | `command_suggestion` | `game.ServerboundCommandSuggestionPacket` | `id`, `command` | raw | 3 |  |
| 16 | `configuration_acknowledged` | `game.ServerboundConfigurationAcknowledgedPacket` | none | typed | 5 | Answer to Start Configuration. The client never sends it at a switch if Start Configuration is held back. |
| 17 | `container_button_click` | `game.ServerboundContainerButtonClickPacket` | `containerId`, `buttonId` | typed | 3 |  |
| 18 | `container_click` | `game.ServerboundContainerClickPacket` | `containerId`, `stateId`, `slotNum`, `buttonNum`, `containerInput`, `changedSlots`, `carriedItem` | typed | 3 | `containerId` and `stateId` belong to the server. |
| 19 | `container_close` | `game.ServerboundContainerClosePacket` | `containerId` | typed | 3 |  |
| 20 | `container_slot_state_changed` | `game.ServerboundContainerSlotStateChangedPacket` | `slotId`, `containerId`, `newState` | typed | 3 |  |
| 21 | `cookie_response` | `cookie.ServerboundCookieResponsePacket` | `key`, `payload` | typed | 4 |  |
| 22 | `custom_payload` | `common.ServerboundCustomPayloadPacket` | `payload` | typed | 4 |  |
| 23 | `debug_subscription_request` | `game.ServerboundDebugSubscriptionRequestPacket` | `subscriptions` | raw | 4 | Debug only. |
| 24 | `edit_book` | `game.ServerboundEditBookPacket` | `slot`, `pages`, `title` | typed | 4 |  |
| 25 | `entity_tag_query` | `game.ServerboundEntityTagQueryPacket` | `transactionId`, `entityId` | typed | 1,3 |  |
| 26 | `interact` | `game.ServerboundInteractPacket` | `entityId`, `hand`, `location`, `usingSecondaryAction` | typed | 1 |  |
| 27 | `jigsaw_generate` | `game.ServerboundJigsawGeneratePacket` | `pos`, `levels`, `keepJigsaws` | typed | 4 |  |
| 28 | `keep_alive` | `common.ServerboundKeepAlivePacket` | `id` | typed | 3 |  |
| 29 | `lock_difficulty` | `game.ServerboundLockDifficultyPacket` | `locked` | typed | 4 |  |
| 30 | `move_player_pos` | `game.ServerboundMovePlayerPacket.Pos` | `x/y/z`, `yRot/xRot`, `onGround,horizontalCollision` | typed | 4 | The proxy reads it to know where the player is. |
| 31 | `move_player_pos_rot` | `game.ServerboundMovePlayerPacket.PosRot` | `x/y/z`, `yRot/xRot`, `onGround,horizontalCollision` | typed | 4 | The proxy reads it to know where the player is. |
| 32 | `move_player_rot` | `game.ServerboundMovePlayerPacket.Rot` | `x/y/z`, `yRot/xRot`, `onGround,horizontalCollision` | typed | 4 |  |
| 33 | `move_player_status_only` | `game.ServerboundMovePlayerPacket.StatusOnly` | `x/y/z`, `yRot/xRot`, `onGround,horizontalCollision` | typed | 4 |  |
| 34 | `move_vehicle` | `game.ServerboundMoveVehiclePacket` | `movingTo`, `onGround` | typed | 4 |  |
| 35 | `paddle_boat` | `game.ServerboundPaddleBoatPacket` | `left`, `right` | typed | 4 |  |
| 36 | `pick_item_from_block` | `game.ServerboundPickItemFromBlockPacket` | `pos`, `includeData` | typed | 4 |  |
| 37 | `pick_item_from_entity` | `game.ServerboundPickItemFromEntityPacket` | `id`, `includeData` | typed | 1 |  |
| 38 | `ping_request` | `ping.ServerboundPingRequestPacket` | `time` | typed | 3 | Answered by Pong Response. |
| 39 | `place_recipe` | `game.ServerboundPlaceRecipePacket` | `containerId`, `recipe`, `useMaxItems` | typed | 3 |  |
| 40 | `player_abilities` | `game.ServerboundPlayerAbilitiesPacket` | `isFlying` | typed | 4 |  |
| 41 | `player_action` | `game.ServerboundPlayerActionPacket` | `pos`, `direction`, `action`, `sequence` | typed | 3 | `sequence` is answered by Block Changed Ack. |
| 42 | `player_command` | `game.ServerboundPlayerCommandPacket` | `id`, `action`, `data` | typed | 1 | `id` is the player's own entity id. |
| 43 | `player_input` | `game.ServerboundPlayerInputPacket` | `input` | typed | 4 |  |
| 44 | `player_loaded` | `game.ServerboundPlayerLoadedPacket` | none | typed | 3 | Sent once the client finished loading a world. After a hidden switch the client does not send it again; whether the new server needs it is not verified. |
| 45 | `pong` | `common.ServerboundPongPacket` | `id` | typed | 3 |  |
| 46 | `punch` | `game.ServerboundPunchPacket` | none | typed | 4 |  |
| 47 | `recipe_book_change_settings` | `game.ServerboundRecipeBookChangeSettingsPacket` | `bookType`, `isOpen`, `isFiltering` | typed | 4 |  |
| 48 | `recipe_book_seen_recipe` | `game.ServerboundRecipeBookSeenRecipePacket` | `recipe` | raw | 4 |  |
| 49 | `rename_item` | `game.ServerboundRenameItemPacket` | `name` | typed | 4 |  |
| 50 | `resource_pack` | `common.ServerboundResourcePackPacket` | `id`, `action` | typed | 3 |  |
| 51 | `seen_advancements` | `game.ServerboundSeenAdvancementsPacket` | `action`, `tab` | raw | 4 |  |
| 52 | `select_trade` | `game.ServerboundSelectTradePacket` | `item` | typed | 4 |  |
| 53 | `set_beacon` | `game.ServerboundSetBeaconPacket` | `primary`, `secondary` | typed | 4 |  |
| 54 | `set_carried_item` | `game.ServerboundSetCarriedItemPacket` | `slot` | typed | 4 |  |
| 55 | `set_command_block` | `game.ServerboundSetCommandBlockPacket` | `pos`, `command`, `trackOutput`, `conditional`, `automatic`, `mode` | typed | 4 |  |
| 56 | `set_command_minecart` | `game.ServerboundSetCommandMinecartPacket` | `entity`, `command`, `trackOutput` | typed | 1 |  |
| 57 | `set_creative_mode_slot` | `game.ServerboundSetCreativeModeSlotPacket` | `slotNum`, `itemStack` | typed | 4 |  |
| 58 | `set_game_rule` | `game.ServerboundSetGameRulePacket` | `entries` | typed | 4 |  |
| 59 | `set_jigsaw_block` | `game.ServerboundSetJigsawBlockPacket` | `pos`, `name`, `target`, `pool`, `finalState`, `joint`, `selectionPriority`, `placementPriority` | typed | 4 |  |
| 60 | `set_structure_block` | `game.ServerboundSetStructureBlockPacket` | `pos`, `updateType`, `mode`, `name`, `offset`, `size`, `mirror`, `rotation`, `data`, `ignoreEntities`, `strict`, `showAir`, `showBoundingBox`, `integrity`, `seed` | typed | 4 |  |
| 61 | `set_test_block` | `game.ServerboundSetTestBlockPacket` | `position`, `mode`, `message` | typed | 4 |  |
| 62 | `sign_update` | `game.ServerboundSignUpdatePacket` | `pos`, `lines`, `slot` | typed | 4 |  |
| 63 | `spectator_action` | `game.ServerboundSpectatorActionPacket` | `spectateEntityId` | typed | 1 |  |
| 64 | `teleport_to_entity` | `game.ServerboundTeleportToEntityPacket` | `uuid` | typed | 4 | Targets an entity by UUID, not by id. |
| 65 | `test_instance_block_action` | `game.ServerboundTestInstanceBlockActionPacket` | `pos`, `action`, `data` | typed | 4 |  |
| 66 | `use_item_on` | `game.ServerboundUseItemOnPacket` | `hand`, `hitResult`, `sequence` | typed | 3 | `sequence` is answered by Block Changed Ack. |
| 67 | `use_item` | `game.ServerboundUseItemPacket` | `hand`, `sequence`, `yRot`, `xRot` | typed | 3 | `sequence` is answered by Block Changed Ack. |
| 68 | `custom_click_action` | `common.ServerboundCustomClickActionPacket` | `id`, `payload` | typed | 4 |  |

## Switch checklist

### Packets the proxy holds back (category 5)

These packets would show the switch to the player. During a switch the proxy drops them or answers them itself:

- **Login** (`play/clientbound` 50): the client builds a new world and waits for it to load (`ClientPacketListener.handleLogin`). Only the first Login of the client's session reaches the client.
- **Respawn** (`play/clientbound` 84): the client builds a new world and waits for it to load (`ClientPacketListener.handleRespawn`).
- **Start Configuration** (`play/clientbound` 120): the client clears its world and shows the reconfiguration screen (`ClientPacketListener.handleConfigurationStart`). This means the whole Configuration state is out of reach during a switch. The two servers must therefore send the same registries, tags and enabled features (Registry Data, Update Tags, Update Enabled Features, Select Known Packs), because the client cannot receive new ones without a visible reconfiguration.
- **Game Event 13** `LEVEL_CHUNKS_LOAD_START` (`play/clientbound` 39): part of a world load (`ClientPacketListener.handleGameEvent`). Other Game Events (weather, game mode) still pass.
- **Transfer** (`play/clientbound` 132, `configuration/clientbound` 12): the client disconnects and opens the connect screen (`ClientCommonPacketListenerImpl.handleTransfer`).
- **Disconnect** (`play/clientbound` 32, `configuration/clientbound` 2) from the old server: the client stays connected to the proxy.
- **Finish Configuration** and **Configuration Acknowledged**: they only appear around a configuration phase, which a switch never starts.

### Entity ids to rewrite (category 1)

Clientbound packets with an entity id: Add Entity (and its `data` for projectile owners), Animate, Block Destruction, Damage Event, Debug Entity Value, Entity Event, Entity Position Sync, Hurt Animation, Login (`playerId`), Mount Screen Open, the three Move Entity packets, Move Minecart Along Track, Player Combat Kill, Player Look At, Projectile Power, Remove Entities, Remove Mob Effect, Rotate Head, Set Camera, Set Entity Data (and some metadata values), Set Entity Link, Set Entity Motion, Set Equipment, Set Passengers, Sound Entity, Swing Animation, Take Item Entity, Teleport Entity, Update Attributes, Update Mob Effect.

Serverbound packets with an entity id: Attack, Entity Tag Query, Interact, Pick Item From Entity, Player Command, Set Command Minecart, Spectator Action.

Packets that name entities by UUID (Boss Event, Player Info, Teleport To Entity, Waypoint) or by name (Set Player Team, Set Score) need no id rewrite.

### State in the ghost (category 2)

Every category 2 packet the proxy forwards updates the ghost. During the prepare phase of a switch (DESIGN.md section 5), the target's category 2 packets go into the target view and are not forwarded. At the commit, the delta is sent as one bundle. For this inventory that means:

- removals come from the packets that already exist for them: Remove Entities, Boss Event (remove), Player Info Remove, Set Objective (remove), Set Player Team (remove), Reset Score, Remove Mob Effect, Forget Level Chunk, Recipe Book Remove, Update Advancements;
- additions and changes are sent with the same packets the target used, with entity ids rewritten: chunks, entities, inventory slots and cursor, held slot, health, food, XP, abilities, effects, attributes, time, weather (Game Event 1, 2, 7, 8), world border, spawn point, difficulty, tick rate, tab list, boss bars and scoreboard;
- the player's position comes from a Player Position whose teleport id the proxy chooses;
- an open container is closed by the switch in the first version (Container Close).

Packets that need no ghost entry: titles, action bar, sounds, particles and level events. Whatever is playing finishes by itself.

### Session state (category 3)

These follow DESIGN.md section 7:

- **Keep-alive** (both states) and **Ping/Pong**: the proxy keeps the client alive with its own ids and each backend separately. A client keep-alive is never forwarded.
- **Teleport ids** (Player Position, Accept Teleportation): the proxy numbers them and routes each answer to the backend that asked. An answer to an old id is dropped after a switch.
- **Block-change sequences** (Player Action, Use Item, Use Item On, Block Changed Ack): one counter for the client, mapped per backend. The proxy acknowledges the old server's pending sequences itself at the switch.
- **Containers** (Open Screen, Container Set Content/Slot/Data, Container Click, Container Close, Merchant Offers, Place Ghost Recipe, Mount Screen Open): window ids and state ids are translated.
- **Transaction ids** (Tag Query and the two tag queries, Command Suggestions): each answer goes back to the backend that asked.
- **Chunk batches** (Chunk Batch Start/Finished, Chunk Batch Received): reports go to the active backend; the proxy answers the inactive one's batch markers.
- **Bundles** (Bundle Delimiter): a bundle from a backend is never cut or mixed with another backend's packets.
- **Signed chat** (Player Chat, Chat, Chat Command Signed, Chat Ack, Delete Chat, Chat Session Update): the first version needs backends that do not enforce signed chat. The proxy passes the player's Chat Session Update to the new backend.
- **Player Loaded** (serverbound): the client sends it once a world finished loading, so it does not send it again after a hidden switch. Whether the target needs it before it treats the player as loaded is not verified.
