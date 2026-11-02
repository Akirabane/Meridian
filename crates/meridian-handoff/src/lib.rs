//! The switch of a player from one backend to another while their client stays
//! connected to the proxy (DESIGN.md section 5).
//!
//! - [`Session`] relays a player's session like `meridian-net`'s relay, feeds the
//!   ghost on the way, and can change the backend it relays to.
//! - [`switch()`] runs the four steps against two backends' [`PlayerControl`]s.
//! - [`PlayerControl`] is the player contract of the sidecar (`meridian-sidecar`),
//!   which [`meridian_sidecar::client::SidecarClient`] implements.
//!
//! # The four steps
//!
//! 1. **Prepare.** The target is told to expect the player (`player-expect`), and
//!    the proxy logs the player in on it over a new connection. The proxy plays the
//!    target's configuration phase as the client played its own, sending what the
//!    client sent ([`ClientConfiguration`]), and hashes what the target sends
//!    ([`Digest`]). In the play phase the target's packets go into a target view,
//!    in the client's numbering, and the proxy answers keep-alives, pings,
//!    teleports and chunk batches, and says the player has loaded. Nothing reaches
//!    the client. The world is taken as sent once a chunk batch has come and no
//!    other followed for a short while.
//! 2. **Freeze.** The source freezes the player and gives their record
//!    (`player-export freeze`). The client still gets the source's packets.
//! 3. **Hand over.** The target takes the record without placing the player
//!    (`player-import` with `"place": false`) and sends the held player's
//!    connection what the import changes. The record's position is left out by
//!    default ([`Settings::carry_position`]): the player arrives where the target
//!    put them, in the chunks it already sent. With it, the target moves the
//!    player and streams the chunks around the new place over the next ticks. The
//!    delta waits for the position the import sends, and with a carried position
//!    for the chunk batches to stop for [`Settings::settle`], so the target view is
//!    the world after the import. It never waits for the target to be quiet: a
//!    busy server never is, and the player is frozen meanwhile. Then, in one go
//!    under the
//!    session's locks: the delta from the ghost to the target view goes to the
//!    client, then the target's command tree, recipes, advancements and the
//!    player's own synced data; the target becomes the active backend. Last, the
//!    target places the player (`player-confirm`).
//! 4. **Commit.** The source lets the player go (`player-release moved`) and its
//!    connection closes. Its last packets, the disconnect among them, never reach
//!    the client.
//!
//! Before the first packet of the delta, any failure leaves the player where they
//! were: the target forgets them (`player-drop`, which also cancels an import not
//! yet confirmed, with nothing saved or said), the source lets them go on
//! (`player-release abort`) if it froze them, and the client saw nothing. If the
//! target will not place the player after the delta, the client already holds the
//! target's world: the player is disconnected with a message, and the source keeps
//! them. A player who leaves once their record is out of the source has it saved
//! on the target as a normal save (decision 10).
//!
//! # The digest is checked on the prepare connection itself
//!
//! The client went through one configuration phase, with its first backend. A
//! switch does not give it another, so the target's registry data, tags, enabled
//! features, known packs and code of conduct must be those the client holds. They
//! are hashed as the target sends them on the prepare connection, and compared with
//! the client's before anything else. A digest is kept per backend: a backend
//! known to differ is refused before it is asked anything.
//!
//! Checking on the prepare connection never puts the player in the target's world.
//! Horizon's documentation of the held player (`docs/server.md`, "Players arriving
//! from another server", fork `feat/held-player` at 190362346) says a player who
//! logs in after `player-expect` gets the join sequence and the level around it,
//! "but it is not placed: no join message, no player-list entry for the others, not
//! among the entities they are shown, not simulated, and nothing else it sends is
//! acted on", until `player-import`. A target refused on its digest gets a
//! `player-drop`, which disconnects the held player "without a save or a word to
//! anyone".
//!
//! Decision 4 is checked as well, before the freeze: the target must put the
//! player in the dimension they are in, and enforce signed chat as the first
//! backend does. The record's dimension is checked again after the export.
//!
//! # Timing
//!
//! Each step is told to [`Settings::trace`] with the time since the switch began,
//! for the proxy's log.
//!
//! Horizon lets a freeze go after some seconds (ten, then thirty), and a held player nobody takes
//! after a minute; `player-expect` holds a place for 60 seconds at most. The
//! freeze, the import and the hand-over to the client must fit in the first of
//! these: the world is fetched before the freeze, so what is left is three requests
//! and the delta.
//!
//! # Known limits
//!
//! - **A player who leaves after the import** is placed on the target
//!   (`player-confirm`) so that it saves them; its players see them arrive and
//!   leave at once when the arrival is announced.
//! - **The client's packets in flight.** Between the hand-over and the moment the
//!   client reads the delta, what the client sends goes to the target. Moves are
//!   ignored there until the client confirms the position of the delta; other
//!   packets (a click on an entity of the old world) reach the target as they are.
//! - **Resource packs.** A target that sends a resource pack in its configuration
//!   phase is refused.
//! - **The client's view distance** is that of its Client Information, sent again
//!   to the target; a change of it during the session is not.
//! - **The world is taken as sent** on a quiet period after a chunk batch, not on
//!   a count of columns.
//! - What the ghost does not keep (`meridian-ghost`'s known limits) is not in the
//!   delta, except the packets sent after it.
mod configure;
mod control;
mod prepare;
mod session;
mod switch;

pub use configure::{ClientConfiguration, Digest, DigestBuilder, configure};
pub use control::{Arrival, Imported, Outcome, PlayerControl, Record, record_dimension, uuid_text};
pub use session::{Session, SessionHandle};
pub use switch::{
    After, Digests, Done, Failure, PARTS, Settings, Step, TargetBackend, Trace, switch,
};

#[cfg(test)]
mod tests;
