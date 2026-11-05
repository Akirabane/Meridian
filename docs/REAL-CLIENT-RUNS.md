# Runs with a real client

This page records what was seen when a real Horizon client played through Meridian in front of
real Horizon servers. It exists because those runs are written down in only one place: a public
comment by Akirabane on the Horizon issue
[horizon-rust/horizon#7484](https://github.com/horizon-rust/horizon/issues/7484#issuecomment-6644342145),
posted on 4 November 2026 at 22:59 UTC (23:59 in Paris) and edited at 23:01 UTC. This page adds
nothing to that comment and to the other pages of this repository. Where the comment does not
say something that a reader would want to know, the cell says **to be confirmed by Akirabane**
instead of guessing.

**These are the runs of one person, on one machine (`localhost`), with one client. They are not
a run on a real network, not a load test, and they were not repeated or recorded in logs kept in
this repository. The proxy log lines below are quoted from the comment.**

## The setup, as the comment tells it

| | |
| --- | --- |
| Machine | One machine, everything on `localhost`. The operating system is not stated: to be confirmed by Akirabane |
| Horizon servers | Three, called `game1` and `game2` in the comment (the name of the third is not given: to be confirmed by Akirabane). A build "of the #7484 stack, one local commit before the heads pushed now". The only difference with the pushed head of Horizon's #8742 is a two-line fix in `crates/horizon-client/src/game/dedicated.rs`, "checked with `git diff`". The exact Horizon commit is not given: to be confirmed by Akirabane |
| Horizon client | One, in offline mode. Its version or build is not given: to be confirmed by Akirabane |
| Meridian | The proxy, and one sidecar per server. The Meridian commit is not given. The permission-level bug of the first run was seen before [#70](https://github.com/Akirabane/Meridian/pull/70) (merged as `e49cf52`) and the fix was checked with the same client afterwards; the fallback runs concern the code of [#84](https://github.com/Akirabane/Meridian/pull/84) (merged as `9ad6b25`, "merged tonight" in the comment). Which commit each run used: to be confirmed by Akirabane |
| Settings named in the comment | `switch-carry-position` off (the position is not carried until Horizon's #8735 is in). Anything else, the proxy's `meridian.toml` included, is not stated: to be confirmed by Akirabane |

## The runs

| Run | Date | Configuration | What was observed | Duration measured | Limits |
| --- | --- | --- | --- | --- | --- |
| 1. Moves between servers with a real client | Not stated. Before the comment of 4 November 2026: to be confirmed by Akirabane | The setup above. The player was moved by `/play`, `/hub`, `/server <name>`, the console's `switch` and `drain <server> --move` | The player stayed in the game, "with inventory, experience, health and hunger following, and a very slight rollback, because the position is not carried yet". No disconnection is reported | "Between about 0.6 and 1.4 seconds" for a move, in the proxy's log. How many moves, and the time by method, are not given: to be confirmed by Akirabane | The position is not carried (see above). One client, one machine. The comment gives no counts of moves and no log |
| 2. Permission level after a move | Not stated | As run 1 | After a move the permission level stayed at 0, and F3+F4 was refused. Meridian forwards the level since [#70](https://github.com/Akirabane/Meridian/pull/70), "and the same client confirmed it" | Not applicable | The comment does not say how many times it was tried after the fix |
| 3. A server killed under the player | About 23:57, Paris time, on the night of 4 to 5 November 2026 ("tonight at about 23:57 (Paris)") | The setup above, with the player on `game2`. `game2`'s process was killed (how it was killed is not stated: to be confirmed by Akirabane) | The proxy logged `lost backend "game2" (the connection was lost ...); moved to "game1"`. The player landed on `game1` "without being disconnected, with a chat line saying so, and the proxy's counts followed". The comment does not quote that chat line; [OPERATING.md](OPERATING.md), section 5.11 gives the wording the proxy is meant to send | Not given: to be confirmed by Akirabane | This is a cold move: the comment describes the design as nothing exported from the lost server, "the player arrives as a new arrival". What the player saw on screen, such as the loading screen, is not described |
| 4. A server kicks the player | Right after run 3, between about 23:57 and 00:05 Paris time | As run 3, with the command `kick Akirabane test kick from the server console` typed at `game2`'s own console | The proxy logged `lost backend "game2" (it disconnected them: test kick from the server console); moved to "game1"`. The player landed on `game1` "without being disconnected", and the chat showed `You were disconnected from game2: test kick from the server console. You were moved to game1.` | Not given: to be confirmed by Akirabane | As run 3. The chat line was added to the comment at about 00:05 Paris time, after the try |

## What the comment says was not done

- **A run on a real network.** "Everything above is localhost, one machine, one client." Someone
  asked for a run with machines apart; the comment asks what would count for it (for example the
  proxy and two servers on separate machines, with a client over a real network).
- **The proxy's management connection**, which relays a server's own stop announcement, "has never
  been tried against a real Horizon".
- **Carrying the position**, until Horizon's #8735 is in.
- **Undoing a placed arrival on the target side after a failed commit**: still the documented
  limit ([STATE-TRANSFER.md](STATE-TRANSFER.md)).
- **Syncing the proxy's bans with the servers' management protocol** is said to be possible later
  and "not done" in the comment. The repository now has it, off by default
  ([OPERATING.md](OPERATING.md), section 5.12), and says it has not been tried against a real Horizon.
- The sidecar's `status` request is provisional: Horizon does not answer it, and the proxy then
  treats the server as alive. Meridian counts the players itself.

## To be confirmed by Akirabane

1. The date and the time of runs 1 and 2.
2. The operating system and the Horizon client's version or build.
3. The exact Horizon commit of the three servers, and the name of the third.
4. The Meridian commit of each run: before or after #70 for runs 1 and 2, and before or after the
   merge of #84 (`9ad6b25`) for runs 3 and 4.
5. The proxy's configuration: `try` list, balancing and roles, caps, `[fallback]` values, and
   whether the log excerpts above are the only lines of interest.
6. How many moves were made in run 1, how long each kind took (`/play`, `/hub`, `/server`, `switch`,
   `drain --move`), and whether any move failed or was refused.
7. How `game2` was killed in run 3, and the chat line shown to the player there.
8. What the player saw on screen in runs 3 and 4 (the loading screen, the time it took to be back
   in the game).
9. Whether the runs were repeated, and where the full logs are kept, if anywhere.

## What this changes in the other pages

[OPERATING.md](OPERATING.md), [TESTING-FORK.md](TESTING-FORK.md), [PARITY.md](PARITY.md) and the
README, written before this page, say that no run on real servers or with a real client is
recorded. This page records the runs above; it does not make the rest of those statements false:
a real network, many players, the management connection and the other items of the list stay
untried. Those pages are to be reconciled with this one by whoever maintains them.
