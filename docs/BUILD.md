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

Only the root `Cargo.toml` and the CI workflow are shared; each crate has its own
files.

## The protocol dependency

The Minecraft protocol is taken from the `horizon-protocol` crate of Horizon, as a
git dependency pinned to one commit (`[workspace.dependencies]` in the root
`Cargo.toml`). Cargo also builds the two Horizon crates it depends on,
`horizon-core` and `horizon-world`; nothing else of Horizon is built.

Moving the pin is a deliberate change: update the `rev`, run the tests, and note
the commit here.

Pinned commit: `f696adc8996d7d4b11e7d50a863f2735a21c8000`.

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
