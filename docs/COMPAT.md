# Staying compatible with Horizon

Meridian speaks Horizon's Minecraft protocol, and it takes that protocol from
Horizon's own code: the `horizon-protocol` crate, with the two crates it depends on,
`horizon-core` and `horizon-world`. It is pinned to one commit of
[horizon-rust/horizon](https://github.com/horizon-rust/horizon). The commit is
`rev` in `[workspace.dependencies]` of the root `Cargo.toml`, and
[BUILD.md](BUILD.md) repeats it under "Pinned commit". Two lines of that file name the
revision, the one of `horizon-protocol` and the one of `horizon-world` (which
`meridian-handoff` uses directly for the text components), and the two must stay identical:
`scripts/bump-horizon.sh` rewrites both and refuses to go on if one is left behind. By hand,
change both. The script reads and rewrites the lines that have the form
`<name> = { git = "https://github.com/horizon-rust/horizon", rev = "<40 digits>" }`; a third
Horizon dependency written another way (a `branch`, a `tag`, a `[patch]`, another URL) would not be
rewritten, and the script would not notice: write it in that form, or extend the script. Meridian speaks the protocol of
Minecraft Java 26.3 ([PACKETS.md](PACKETS.md)), as the pinned commit does.

The pin keeps a change in Horizon from breaking a build by surprise. It also means a
change of Horizon's protocol is invisible until someone moves the pin. Two things
close that gap: a build every night against Horizon's head, and a script that moves
the pin.

The pin was moved on 2026-11-06 from `f696adc8996d` to `0d21d5520ec2`, Horizon's head that day, with
`scripts/bump-horizon.sh`: Meridian needed no change, and the format check, clippy with
`-D warnings` and `cargo test --workspace` passed (999 tests, 2 ignored).

## The nightly build

The `Horizon compat` workflow (`.github/workflows/horizon-compat.yml`) runs every
night at 03:23 UTC and on demand (Actions tab, "Horizon compat", "Run workflow"). On
Ubuntu it:

1. reads the head commit of Horizon's default branch;
2. replaces the `rev` in `Cargo.toml` of the checkout, with `scripts/bump-horizon.sh`.
   Nothing is committed or pushed;
3. runs `cargo test --workspace`, which builds everything and runs every test against
   that Horizon.

It is a workflow of its own, not a step of the pull request checks, so a red night
never blocks a merge. The workflow's token can read the code and, in a separate job
that does not check out any code, write issues; it has no other right and uses no
secret.

**When it fails**, the job opens one issue, labelled `horizon-compat` and titled
"Horizon compat broken at <commit>". The issue has the Horizon commit, the commit
Meridian is pinned to, the link to the run, the Horizon commits that touched the
protocol crates since the pin, and the first compiler errors or the failed tests. On
the next failing night the same issue is edited (and a comment is added if the Horizon
commit changed), never a second one. **When the build is green again** the job
closes the issue with a comment.

If the run could not read Horizon's head, or the runner failed before the tests ran,
the run is red but the issue is left as it is: that is a problem with the run, not a
finding about Horizon.

### What it costs

Meridian's repository is public, so the minutes of standard runners are free.
Measured on a Windows 11 machine, from an empty target directory, with Cargo limited
to 3 jobs: `scripts/bump-horizon.sh` against Horizon's head (`66f3981f522b`, 2026-11-05)
took 2 minutes 9 seconds in all, fetching the Horizon repository included, and left
2.2 GB in the target directory, which is well inside the 14 GB of a hosted runner.
All of the roughly 900 tests passed. A hosted Ubuntu runner has 4 cores. The workflow has been started by hand twice and both runs
ended green: on 2026-11-05 against `66f3981f522b`, and on 2026-11-06 against `98af9399cd1c`,
the second about 4 minutes from its start to its last test. The run planned for 03:23 UTC on
2026-11-06 did not take place. The Rust cache (the same
`rust-cache` as the main checks, keyed on the pinned `Cargo.lock`) restores the
crates.io dependencies, so a night builds only the Horizon crates and Meridian's own.
The job stops at 40 minutes. Cargo builds only `horizon-protocol`, `horizon-core` and
`horizon-world` of Horizon, not the server or the client ([BUILD.md](BUILD.md), "What
it costs").

It runs on Linux only. The protocol code does not depend on the platform, and the main
checks already build Windows. Adding Windows would double the time for little gain: add
`windows-latest` to a matrix and have the issue job read the results of both.

### What it does not check

- **No Horizon server is started.** The tests use Meridian's own stand-ins for the
  servers (`meridian-testkit`), built on the new protocol crate. A change in what a
  real server does, as opposed to what its packets look like, is not seen.
- **No real client, no real network.** The same goes for the handoff between servers,
  the sidecar and Horizon's `player-*` requests: they are tested against stand-ins.
- **Only the protocol crates.** A change to anything else of Horizon (the server's
  configuration, the transfer module) is not looked at.
- **Only the head of the default branch.** A feature branch of Horizon is not tried;
  run `scripts/bump-horizon.sh <branch>` on your machine for that.

A green night means "it still builds and the tests still pass". It does not mean that
a real Horizon and Meridian still work together; [RUNNING.md](RUNNING.md) and
[TESTING-FORK.md](TESTING-FORK.md) are about that.

## Moving the pin

```
scripts/bump-horizon.sh              # to the head of Horizon's default branch
scripts/bump-horizon.sh <commit>     # to a given commit, branch or tag
scripts/bump-horizon.sh --no-test    # rewrite and fetch, do not test
```

The script rewrites `rev` on both lines of `Cargo.toml` that name it and the "Pinned commit" line of
[BUILD.md](BUILD.md), lets Cargo update `Cargo.lock`, lists the Horizon commits that
touched the protocol crates between the two, and runs `cargo test --workspace`. When
that fails it prints the first compiler errors and the failed tests. The files keep
the new revision, so the fix can be made in place; `git checkout -- Cargo.toml
Cargo.lock docs/BUILD.md` goes back. When it passes, commit the three files together
with whatever Meridian needed to change. `CARGO_BUILD_JOBS=3` limits the compiler.

On Windows, the script runs as it is in Git Bash (`bash scripts/bump-horizon.sh`). In
PowerShell, the same move by hand is:

```powershell
$sha = (git ls-remote https://github.com/horizon-rust/horizon HEAD).Split("`t")[0]
$text = [System.IO.File]::ReadAllText("$PWD\Cargo.toml")
$text = $text -replace '(horizon-rust/horizon", rev = ")[0-9a-f]{40}', "`${1}$sha"
[System.IO.File]::WriteAllText("$PWD\Cargo.toml", $text, (New-Object System.Text.UTF8Encoding $false))
cargo test --workspace
```

Then change the "Pinned commit" line of [BUILD.md](BUILD.md) to `$sha` by hand. The
`WriteAllText` call keeps the file in UTF-8 without a byte order mark, which Windows
PowerShell 5.1's `Set-Content` would not.

## When the nightly build fails

1. Open the issue. The Horizon commits listed are the ones that touched
   `horizon-protocol`, `horizon-core` or `horizon-world` since the pin; the cause is
   usually one of them.
2. Check that the main checks are green on `main`. If they are red too, the break is
   Meridian's own and not Horizon's.
3. Reproduce it: `scripts/bump-horizon.sh`. A compiler error names a type or a function
   that changed. A failed test names a behaviour that changed.
4. Decide with the Horizon side. Either Meridian follows (fix Meridian, then move the
   pin with the script, in one pull request), or Horizon is asked to change back. If a
   wire format changed, [PACKETS.md](PACKETS.md) and [STATE-TRANSFER.md](STATE-TRANSFER.md)
   may need the same update.
5. The pin stays where it is until then: a red night does not touch `main`. The issue
   closes by itself the first night the build passes, which is also the case once the
   pin has been moved to the commit that broke it and Meridian has been fixed.

## Setting it up

The workflow needs nothing but the repository settings: Issues must be enabled, and
Actions allowed to run workflows. GitHub stops the scheduled workflows of a public
repository that has had no activity for 60 days; the "Run workflow" button (or any
commit) starts them again.
