**One run, on one Windows machine, over the loopback, on 9 November 2026: a real Horizon client types the commands
of an extension through the proxy, a real Horizon server stands behind it, and the server never sees them: not while
the extension is up, not while it is stopped, not when it is back. It is one run, with offline accounts; the limits are
in section 6.**

# An extension's own commands, with a real server and real clients

[EXTENSIONS.md](EXTENSIONS.md), section 4.3, says that a command an extension declares is its own: the proxy takes the
word the player types, asks the extension to run it, and tells the player what it says. The server never sees the word,
also while the extension is down (the player then reads `That command did not answer.`). The tests of the proxy check
what its command hook returns. This page records the same claim with real programs at both ends.

## 1. What ran

| | |
| --- | --- |
| Machine | One Windows PC. Everything on it, over `127.0.0.1`; nothing left it |
| Server | One real `horizon-server` and its engine, Nightly of 5 November 2026 (`66f3981`), 26.3, offline mode |
| Clients | Two real Horizon clients (the same Nightly), minimised windows, offline accounts, driven by `horizon-ctl` |
| Proxy | Meridian built from `main` at `9424fd5`, release build, one backend (the server above), the metrics page on |
| Extension | A small program of the Rust SDK ([EXTENSION-GUIDE.md](EXTENSION-GUIDE.md)), granted `commands` only |
| Its commands | `/party` with the alias `/p` (invite, list, leave: the SDK example's logic), `/time` and `/server` |
| Settings | `[extension.commands] timeout-ms = 1500`; the six commands of the proxy on (`[commands] enabled`) |

The extension declares `/time` and `/server` on purpose, and each runs only to say that it was asked:

- `/time` is a word the Horizon server has too, with an effect that can be read: `time set` moves the server's clock.
  It is the proof word (section 2).
- `/server` is a word the proxy keeps for its own command. The proxy must refuse the extension's declaration of it, say
  so in its log, and go on answering the word itself.

Every run the extension is asked is written to its own log with the word and the arguments, so that a command it got is
told from one it did not.

## 2. How the claim is proved: by its effect

Horizon logs no command a player types, so a server log cannot show that the server did not see a word: "no line" would
read the same whether it saw it or not. The run proves it by **effect**:

1. The extension declares `/time`.
2. An operator of the server types `/time set 18000` as a player.
3. The server's clock (`time query time` on its console, 26.3's form) is read afterwards. It reaches 18000 only if the
   server got the command.

A first part with **no extension** is the control: there the clock does move. Without it, an unmoved clock could mean
that the command was never allowed, or that the clock was not read right.

The chat the player reads is the second evidence (the extension's lines, or `That command did not answer.`, and no
"unknown command" from the server), and the extension's own log is the third (what it was asked, and when not).

## 3. The four parts

The clock was set to 1,000 ticks before each part, a few seconds before the command was typed.

### A. Control: no extension

| Id | What the player did | What was read | Result |
| --- | --- | --- | --- |
| A0 | A client joins through the proxy | It joined | Passed |
| A2 | The operator types `/time set 18000` | The clock read 18,050 | Passed: the server sees the word |

With no extension, the second command `/party list` printed nothing the player could read. The client or the server may
say nothing for a word nobody has, so that line is kept as a note, not as a check: the clock is the proof.

### B. The extension is up

| Id | What the player did | What was read | Result |
| --- | --- | --- | --- |
| B0 | (the proxy starts) | It connected, granted `commands`; its log refused the extension's `/server` | Passed |
| B1 | `/party invite <the other player>` | The extension's two lines, no "unknown" from the server | Passed |
| B2 | `/p list` | The same command through its alias | Passed |
| B3 | `/time set 18000` | The extension's line "the server did not get it"; the clock read 1,156 | Passed |
| B4 | `/list`, a word of the server | The server answered; the extension was not asked | Passed |
| B5 | `/msg <the other player> ...` | The line reached the other player | Passed |
| B6 | `/server`, a word of the proxy | The proxy answered it; the extension was not asked | Passed |
| B7 | (the metrics page) | `meridian_extension_requests_total{outcome="answered"}` went from 0 to 3 | Passed |

B3 is the one that matters: `/time set 18000` with the extension up left the clock where it was.

### C. The extension is stopped (the program is ended, by its own process)

| Id | What the player did | What was read | Result |
| --- | --- | --- | --- |
| C1 | `/party list` | `That command did not answer.`, no "unknown" from the server | Passed |
| C2 | `/time set 18000` | `That command did not answer.`; the clock read 1,164 | Passed |
| C3 | `/msg ...` | Still reached the other player | Passed |
| C4 | (the extension's log) | The stopped extension logged no run | Passed |
| C5 | (the proxy's log) | It said the connection ended and that it would try again | Passed |

C2 is the second one that matters: with the extension gone, the word `/time` stayed the extension's and did not fall
through to the server.

### D. The extension is back (a new process)

| Id | What the player did | What was read | Result |
| --- | --- | --- | --- |
| D0 | (the extension starts again) | The proxy connected again: a second hello | Passed |
| D1 | `/party list` | `You lead no party.` (a new process: the party of B1 was kept in memory and is gone) | Passed |
| D2 | `/time set 18000` | The extension's line; the clock read 1,104 | Passed |
| D3 | `/msg ...` | Still reached the other player | Passed |

### Then

| Id | Check | Result |
| --- | --- | --- |
| E1 | The extension's token is in no log of the run (the proxy's, the extension's, the clients') | Passed |
| S0 | The Horizon server was up before the parts began | Passed |

**22 checks, 22 passed.** The extension's own log holds five runs: the invite, the `list` that came through `/p`, the
`time` of part B, and the `list` and the `time` of part D. It holds none from part C and none from part A. At the end
the metrics page counted five `answered` and two `disconnected` (the two commands typed in part C).

## 4. About the first full run

A first full run read 21 of 23: two of its checks were wrong, not the proxy. One (B6) looked for a sentence that is not
the one in the proxy's log (the line is: the command word `server` is refused, it is a command of the proxy); one (A1)
expected the server to answer an unknown word, which it does not (section 3, part A). The two were corrected, and a
second full run, from a clean start, is the record above. Every other check read the same in both runs.

## 5. How to run it again

The scripts that drove this run are not in this repository: they name folders of one machine. The steps are:

1. **A server.** `horizon-server init` with `--offline` and `--op <the operator>`, then `horizon-server start`, which
   leaves the server running in the background. Do not wait for its output through a pipe: the server keeps the pipe
   open and the call does not return. `horizon-server cmd "time query time"` reads the clock (in Horizon 26.3 the old
   `time query daytime` no longer exists).
2. **The proxy.** `auth-mode = "offline"`, one `[[backends]]`, `forwarding-secret` as the server's, `[commands] enabled`
   as above and, for the second part, an `[[extension]]` with `grant = ["commands"]`, a `token-file` and
   `[extension.commands] timeout-ms = 1500` ([CONFIG.md](CONFIG.md)). `[metrics]` on, for B7.
3. **The extension.** The SDK's `command_party` example (`crates/meridian-extension/examples`) declares `/party` and
   `/p`. The run used a copy that also declares `/time` and `/server`, answers each with a fixed line, and writes
   `<milliseconds> ran <player> /<word> args=<...>` to a log file for every run it is asked.
4. **The clients.** `horizon-client --server 127.0.0.1:<port> --name <name> --control --control-socket <file>`, one for
   the operator and one for the other player. `horizon-ctl --socket <file> command "<line>"` types a command and
   `horizon-ctl --socket <file> messages <n>` reads the chat lines. Wait for the proxy's `joined backend` line before
   typing, and a few seconds after.
5. **The parts**, in the order of section 3. Before each, `time set 1000` on the server console. To stop the extension,
   end its process by the process number the script kept; to bring it back, start it again with the same token.
6. **Clean up.** Stop the clients, the proxy and the extension, `deop` the operator, and stop the server with
   `horizon-server stop`.

Each step is a few lines of a script; the whole run took about six minutes and opened two small client windows.

## 6. What this does not show

- **One run**, on one machine, the loopback only, with the Horizon Nightly of 5 November (`66f3981`).
- **Offline accounts.** The client sends the unsigned form of a command; a signed command, as an online account sends
  it, was not tried.
- **The window before the first hello.** From the start of the proxy until an extension has connected once, its words
  are unknown and go to the server (EXTENSIONS.md, 4.3). The extension was up before the proxy started in part B, so
  that window was not run.
- **A slow extension.** The extension here was ended, so the connection closed at once. An extension that stays
  connected and does not answer in time (the timeout, and then the breaker) was not run; nor was the answer that says
  a player already has a command running.
- **Several extensions** with the same word, and **a reload** that changes the commands: the tests of the proxy cover
  them (`session::tests::extension::own`), the run does not.
- **Linux**, a load, many players, and **Tab completion** of an extension's words (not built).
- **That the clock is the only effect** a command could have on the server. It is the one that was read; the chat the
  second player read (B5, C3, D3) and the server's `/list` (B4) showed no other trace of the extension's words.
