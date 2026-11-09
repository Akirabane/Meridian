# Changes

What changes for whoever runs the proxy, newest first. The proxy has no release number of its own yet (`0.1.0` in
`Cargo.toml`): each entry names the pull requests merged into `main`, and the setting, page or document to read.

## Not released yet: pull request #TBD

### Extensions: the state of each server (#TBD)

No setting changes, and the protocol stays 0: an extension that does not know the new fields and the new event ignores
them. docs/EXTENSIONS.md sections 4.4, 5 and 6.

- **`servers/list` and the candidates of `decide/server`** give three more fields, each absent when unknown: `why` a
  server is `draining` or `not_offered` (`restarting`, `drained`, `not_accepting`, `no_report`), whether it answered the
  proxy's last round of readings (`up`), and what it last said of itself (`report`: its players, capacity, players held
  for a hand-over, places promised, players handed over, whether it is full or stopping, and the age of the reading).
  `up` and `report` are given for a server with a `role` and a sidecar, while `[balancing] enabled` is on.
- **A new event, `servers/state`**, for an extension granted `events`: a server's state for new players changed (its
  word, why, or whether it is stopping), at most once for each server at each round of readings.
- The proxy does not change where it sends players because of any of it: a server that says it is stopping is still
  offered until the restart detector or the balancer says otherwise.

**For whoever writes an extension with the Rust SDK.** `orders::ServerInfo` and `decide::Candidate` have three more
public fields (`why`, `up`, `report`): code that builds one with a struct literal, such as an extension's own tests,
must add them. `events::Event` is now `#[non_exhaustive]`: a `match` on it with no `_` arm no longer compiles; add one
(it also keeps the `match` compiling when a later version adds an event).

## 8 and 9 November 2026: pull requests #151 to #190 (main at `c65004d`)

### Before you update, and going back

Every setting below is off, or keeps the old behaviour, until you write it. A proxy refuses a key it does not know, so a
file that uses one of them does not start on a proxy from before it. To go back to an older proxy, take these keys out
of the file first (the `[extension.*]` tables are those of an `[[extension]]` entry):

| Key | Added by |
| --- | --- |
| `accept-transfers` | #176 |
| `compression-level` | #179 |
| `[extension.commands]` `timeout-ms` | #158 |
| `[extension.orders]` `rate-per-second`, `burst`, `max-running`, `max-kicks-per-minute` | #163 to #173 |
| `[extension.events]` `queue` | #174 |
| `[[backends]]` `state-group` and `seamless-arrival`, `[[forced-host]]` `seamless` | #190 |

An older proxy accepts `grant` naming `commands`, `orders.*` or `events`, and does nothing with them.

One name of the metrics page changed before any release used it: the players sent by another server are counted in
`meridian_incoming_transfers_total{outcome}` (#177). `meridian_transfers_total` keeps its meaning (#182): the seamless
moves.

### Players and servers

- **Worlds that keep their own state, moved between with no loading screen** (#190). `state-group` on a backend: a
  move between two groups carries none of the player's state, and each server keeps its own copy of the player.
  `seamless = true` on a forced host makes a server's vanilla `/transfer` to that host a seamless move; when the move
  cannot be made the transfer goes to the client as before. `seamless-arrival = false` keeps a hardcore world to logins
  in and out, since its hearts and death screen come only with one. Checked locally with real servers and one real
  client; not run on a public proxy. OPERATING.md section 5.15.
- **Players sent by another server** (#176, #177, #185). With `accept-transfers = true` the proxy takes the players a
  server sends with the game's own `/transfer`, checked as any login, the host choosing the server
  (`[[forced-host]]`). Run on a public proxy with a lobby, a survival and a hardcore server, with one player (#186).
  OPERATING.md section 5.13.
- **The compression level towards the players** (#179, #181). `compression-level` (1 to 9, `6` by default, the same
  bytes as before). Level 4 used 11.8 % less processor time on a running proxy at 100 players, for 1.5 to 1.7 % more
  bytes, two runs of each setting, on a PC where other programs used about the same load on both sides; level 3,
  and level 4 at 500 players, gave no conclusive figure (#187). OPERATING.md section 5.14.
- **Fewer writes to each player** (#160, #162). The packets one read of a server brings are written to the player in
  one write; the bytes are the same.
- **The metrics page and `/healthz` answer at once** (#151): about 10 ms where they took 50 to 90 ms.
- **The summary of an evacuation** names the players the fallback had already moved, instead of counting them as left
  behind (#152).

### Extensions

All of it is opt-in: an extension gets only what its entry grants and its hello asks for. The protocol stays 0 (new
methods and fields only). docs/EXTENSIONS.md and docs/EXTENSION-GUIDE.md.

- **An extension's own commands** (#153, #156, #158, #159): a player types `/party`, the proxy asks the extension, and
  the answer goes to the player's chat; the server never sees the command (#188, and with a real server and two real
  clients, #189). The word stays reserved while the extension is configured; when it does not answer, the player
  reads "That command did not answer." Permission nodes `ext.<extension>.<command>`.
- **Orders the extension gives the proxy** (#163 to #173), each behind its own capability and a budget: read the
  players, the servers and the queue (`orders.read`); tell a player or many (`orders.message`); move players
  (`orders.move`); kick a player (`orders.moderate`; node `meridian.kick.exempt` protects someone); drain, undrain or
  reload (`orders.operate`); run a console line (`orders.console`: granting it is giving the console). The SDK has
  the client half (#166).
- **Events the proxy tells the extension** (#174, #178, #180): players joined, left, moved, and moved by the fallback
  when their server was lost; servers up and down; the queue entered and left. A bounded queue for each extension;
  what it drops is counted in `meridian_extension_events_dropped_total` and announced to it.

### Documents

- The load campaigns: 1,000 players on one proxy with simulated servers (docs/LOAD-1000.md, #157, #161), and real
  Horizon servers up to 500 players on one PC, with what gave no result (docs/LOAD-LEVEL2.md, #187).
- The pages that say where things stand were checked against the code, and every series of the metrics page is listed
  (#184; OPERATING.md section 8.1); the online mode's account check with real clients (#175); Horizon's wait at a
  stop (#154).

### Tests and benches only

No change for an operator: #155, #165, #169 (a relay that makes a network worse, for tests), #183, #188.
