# Building

Meridian is a Cargo workspace. It needs Rust 1.85 or later (edition 2024) and
nothing else: there are no system libraries to install.

```
cargo build --release
cargo test --workspace
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
```

The same three checks (format, clippy, tests) run in CI on Linux and on Windows.

## Layout

| Crate | Role |
| --- | --- |
| `meridian-config` | The proxy's settings |
| `meridian-net` | Player and backend connections, and the relay between them |
| `meridian-testkit` | Scripted client and backend, and a packet recorder, for tests |
| `meridian-proxy` | The `meridian` binary |
| `meridian-panel` | A local panel to run a test bench of the proxy and three Horizon servers |
| `meridian-bench` | Measures the proxy on one machine against a direct connection ([BENCH.md](BENCH.md)) |

Only the root `Cargo.toml` and the CI workflow are shared; each crate has its own
files.

## The protocol dependency

The Minecraft protocol is taken from the `horizon-protocol` crate of Horizon, as a
git dependency pinned to one commit (`[workspace.dependencies]` in the root
`Cargo.toml`). Cargo also builds the two Horizon crates it depends on,
`horizon-core` and `horizon-world`; nothing else of Horizon is built. `horizon-world` is also a
direct dependency of `meridian-handoff`, on a second line of the same `Cargo.toml` with the same
`rev`; the two lines are kept identical ([COMPAT.md](COMPAT.md)).

Moving the pin is a deliberate change: `scripts/bump-horizon.sh` updates the `rev`,
runs the tests and notes the commit here. A nightly build against Horizon's head warns
when a change of Horizon would break Meridian; [COMPAT.md](COMPAT.md) describes both.

Pinned commit: `0d21d5520ec26c49b774bf1533e53ea8af81278d`.

### What it costs

Measured on Windows 11 with 32 logical processors, from an empty target
directory, with the git sources already fetched:

| Step | Time | Target directory after |
| --- | --- | --- |
| `cargo check --workspace --all-targets` | 57 s | 81 MB |
| `cargo test --workspace` (builds the test profile) | 26 s more | 605 MB |
| `cargo clippy` after the check | under 1 s | unchanged |

Fetching the Horizon repository once into Cargo's git cache is the other cost: about
140 MB for the repository, plus the checkout of the pinned commit, and the time
depends on the connection. A second build of an unchanged tree takes a few
seconds.

## Sharing a target directory

Several people building Meridian at once can point `CARGO_TARGET_DIR` at one
folder. Cargo locks the folder while it builds, so builds queue instead of
multiplying the work and the disk use. Prefer `cargo check` when a full build is
not needed.

## Linux binaries from CI

To try Meridian on a Linux machine that has no compiler, the `Linux binaries` workflow
builds the programs on request. It never runs by itself, and it makes no release, no
tag and no image.

```
gh workflow run linux-binaries.yml            # on the default branch
gh workflow run linux-binaries.yml --ref my-branch
gh run list --workflow linux-binaries.yml     # find the run id
gh run download <id>                          # saves the artifact in the current folder
```

The same can be done from the Actions tab (Linux binaries, Run workflow) and the page
of the run. The artifact is private to the people who can read the repository and is
kept for 14 days.

It holds `meridian-linux-x86_64-<commit>.tar.gz` (the first 12 digits of the commit) and
`meridian-linux-x86_64-<commit>.tar.gz.sha256`. Check the download, then unpack it:

```
sha256sum --check meridian-linux-x86_64-<commit>.tar.gz.sha256
tar -xzf meridian-linux-x86_64-<commit>.tar.gz
cd meridian-linux-x86_64-<commit>
sha256sum --check SHA256SUMS
```

The folder in the archive has:

| File | What it is |
| --- | --- |
| `meridian` | The proxy |
| `meridian-sidecar` | The sidecar that runs next to each Horizon server |
| `meridian-panel` | The panel that runs a test bench |
| `meridian-bench` | The load measurement ([BENCH.md](BENCH.md)) |
| `meridian.example.toml` | The example configuration |
| `COMMIT.txt` | The full commit, the branch, the run, the version of `rustc`, the date, and the newest glibc the binaries need |
| `SHA256SUMS` | The checksums of the files above |

The binaries are built in release mode for `x86_64-unknown-linux-gnu` on Ubuntu
22.04, whose glibc (2.35) is old enough for Debian 12 and 13 and Ubuntu 22.04 and
later; the workflow fails if a binary asks for a glibc newer than Debian 12's 2.36.
They are linked to the system's glibc, not statically and not to musl, so they do not
run on Alpine. They need nothing else: no compiler, no Rust, no other library. The
build uses `--locked`, so the dependencies are those of `Cargo.lock`.

## Releases

A release is made by pushing a tag `vX.Y.Z` whose version is the workspace's
(`version` in the root `Cargo.toml`). The `Release` workflow then:

1. builds `meridian` in the release profile for `x86_64-unknown-linux-gnu` and
   `x86_64-pc-windows-msvc`, runs the tests, and packs each binary with the README,
   the licence and the example configuration (`.tar.gz` for Linux, `.zip` for
   Windows);
2. unpacks each archive on a clean runner, runs `meridian --check
   meridian.example.toml`, and checks that `meridian --version` matches the tag;
3. writes `SHA256SUMS`, records a build provenance attestation for the archives and
   the checksums, and attaches them to a draft release, which is published by hand.

To check a download:

```
sha256sum -c SHA256SUMS --ignore-missing
gh attestation verify meridian-v0.1.0-linux-x86_64.tar.gz -R Akirabane/Meridian
```

The attestation says that the file was built by this repository's workflow from the
tagged commit. It is signed with a short-lived certificate from GitHub's own
signing service, so the repository holds no key. It does not say that the code is
safe, only where the file comes from.

The Linux binary is linked against the GNU C library of the runner that built it, so
it needs a distribution at least as recent as the one GitHub's `ubuntu-latest`
runner uses.
