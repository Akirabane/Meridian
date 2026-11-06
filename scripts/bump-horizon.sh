#!/usr/bin/env bash
# Move the Horizon revision Meridian is built against, run the tests, and say what broke.
#
#   scripts/bump-horizon.sh              head of Horizon's default branch
#   scripts/bump-horizon.sh REV          a commit (40 hexadecimal digits), a branch or a tag
#   scripts/bump-horizon.sh --no-test    rewrite the revision and fetch it, but do not test
#
# It rewrites `rev` in the root Cargo.toml, the "Pinned commit" line of docs/BUILD.md
# and Cargo.lock, then runs `cargo test --workspace`. The files stay changed when the
# tests fail, so the breakage can be fixed in place; `git checkout -- Cargo.toml
# Cargo.lock docs/BUILD.md` puts the old revision back.
#
# Set BUMP_REPORT to a file name to get what the run found as Markdown (the nightly
# workflow does). Set CARGO_BUILD_JOBS to limit the compiler's parallelism.
#
# Exit status: 0 when the tests pass, 1 when the build or a test fails, 2 for a
# revision that cannot be found or an option that is not known.

set -euo pipefail

horizon_url=https://github.com/horizon-rust/horizon
# The Horizon crates the protocol depends on, the ones whose history matters here.
horizon_paths=(crates/horizon-protocol crates/horizon-core crates/horizon-world)

run_tests=1
want=
for arg in "$@"; do
  case $arg in
    --no-test) run_tests=0 ;;
    -h | --help)
      sed -n '2,/^$/p' "$0" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    -*)
      echo "unknown option: $arg" >&2
      exit 2
      ;;
    *)
      if [ -n "$want" ]; then
        echo "only one revision can be given" >&2
        exit 2
      fi
      want=$arg
      ;;
  esac
done

cd "$(dirname "$0")/.."

# The 40 digits of the revision now in Cargo.toml: the first line that names one (the lines of
# horizon-protocol and horizon-world carry the same).
old=$(sed -n 's|.*horizon-rust/horizon", rev = "\([0-9a-f]\{40\}\)".*|\1|p' Cargo.toml | head -n 1)
if [ "${#old}" -ne 40 ]; then
  echo "Cargo.toml has no horizon-protocol line with a 40-digit rev" >&2
  exit 2
fi

# The commit to move to.
if [[ $want =~ ^[0-9a-f]{40}$ ]]; then
  new=$want
else
  if [ -z "$want" ]; then
    found=$(git ls-remote "$horizon_url" HEAD)
  else
    found=$(git ls-remote "$horizon_url" "refs/heads/$want" "refs/tags/$want^{}" "refs/tags/$want")
  fi
  new=$(printf '%s\n' "$found" | sed -n '1s/[[:space:]].*//p')
  if [[ ! $new =~ ^[0-9a-f]{40}$ ]]; then
    echo "cannot find ${want:-the head of the default branch} in $horizon_url" >&2
    exit 2
  fi
fi

echo "Horizon: ${old:0:12} -> ${new:0:12}"
if [ "$old" = "$new" ]; then
  echo "(this is the revision already pinned)"
fi

# Rewrite in place, so the file keeps its mode and its line endings.
rewrite() { # file, sed script
  local tmp
  tmp=$(mktemp)
  sed "$2" "$1" >"$tmp"
  cat "$tmp" >"$1"
  rm -f "$tmp"
}
rewrite Cargo.toml "s|\\(horizon-rust/horizon\", rev = \"\\)[0-9a-f]\\{40\\}|\\1$new|"
rewrite docs/BUILD.md "s|^\\(Pinned commit: \`\\)[0-9a-f]\\{40\\}|\\1$new|"
# Every line that names a Horizon revision (horizon-protocol and horizon-world) must now carry it.
total=$(grep -c 'horizon-rust/horizon", rev = "' Cargo.toml || true)
moved=$(grep -c "horizon-rust/horizon\", rev = \"$new\"" Cargo.toml || true)
if [ "$total" -lt 1 ] || [ "$total" -ne "$moved" ]; then
  echo "Cargo.toml was not rewritten on every line that names a Horizon revision ($moved of $total)" >&2
  exit 2
fi

# Resolve and download the new revision; this also rewrites Cargo.lock.
cargo fetch

report=${BUMP_REPORT:-}
[ -z "$report" ] || : >"$report"
# Print to the terminal and, if asked for, to the report.
say() {
  printf '%s\n' "$*"
  if [ -n "$report" ]; then printf '%s\n' "$*" >>"$report"; fi
}

say "Horizon ${old:0:12} -> ${new:0:12}: $horizon_url/compare/${old:0:12}...${new:0:12}"
say ""
say "Commits that touch the crates Meridian builds (${horizon_paths[*]}):"
say ""
history=
for db in "${CARGO_HOME:-$HOME/.cargo}"/git/db/horizon-*; do
  [ -d "$db" ] || continue
  if git --git-dir="$db" cat-file -e "$old^{commit}" 2>/dev/null &&
    git --git-dir="$db" cat-file -e "$new^{commit}" 2>/dev/null; then
    history=$(git --git-dir="$db" log --oneline --no-decorate "$old..$new" -- "${horizon_paths[@]}")
    break
  fi
done
if [ -z "$history" ]; then
  say "(none, or the history is not in Cargo's git cache)"
else
  say "$(printf '%s\n' "$history" | sed -n '1,20p' | sed 's/^/    /')"
  count=$(printf '%s\n' "$history" | wc -l | tr -d ' ')
  if [ "$count" -gt 20 ]; then say "    ... and $((count - 20)) more"; fi
fi
say ""

if [ "$run_tests" -eq 0 ]; then
  say "Not tested (--no-test)."
  exit 0
fi

log=$(mktemp)
trap 'rm -f "$log"' EXIT
if cargo test --workspace 2>&1 | tee "$log"; then
  say "Build and tests pass against ${new:0:12}."
  say ""
  say "To keep it: commit Cargo.toml, Cargo.lock and docs/BUILD.md."
  exit 0
fi

# Show what broke: the first compiler errors, then the tests that failed.
say "Build or tests fail against ${new:0:12}."
say ""
errors=$(grep -E -A8 -m3 '^error' "$log" | head -c 3000 || true)
failed=$(grep -E '^test .* FAILED$|^---- .* ----$' "$log" | sed -n '1,20p' || true)
{
  if [ -n "$errors" ]; then
    echo "Compiler errors (first three):"
    echo
    echo '```'
    printf '%s\n' "$errors" | sed 's/```/'"'''"'/g'
    echo '```'
  fi
  if [ -n "$failed" ]; then
    echo
    echo "Failed tests:"
    echo
    echo '```'
    printf '%s\n' "$failed" | sed 's/```/'"'''"'/g'
    echo '```'
  fi
  if [ -z "$errors" ] && [ -z "$failed" ]; then
    echo "The log has no compiler error and no failed test; its last lines:"
    echo
    echo '```'
    tail -n 15 "$log" | sed 's/```/'"'''"'/g'
    echo '```'
  fi
} | while IFS= read -r line; do say "$line"; done
say ""
say "The files keep the new revision. To go back: git checkout -- Cargo.toml Cargo.lock docs/BUILD.md"
exit 1
