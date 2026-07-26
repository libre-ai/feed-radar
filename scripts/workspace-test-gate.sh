#!/usr/bin/env bash
# Runs the whole Rust workspace test suite and declares what it did not run.
#
# Until 2026-07-26 the only `cargo test` in CI was the one in
# `.github/workflows/jwt-crypto-provider.yml`, filtered to
# `-p feedmind-api --bin feedmind-api routes::auth::tests`. That job exists to
# prove one thing — that a session token can be signed and verified — and it is
# deliberately narrow. Every other test in the workspace, 102 of them across
# eleven members, ran nowhere: a member could go red and no required check would
# notice. This gate closes that gap.
#
# `--all-targets --all-features` rather than a bare `cargo test`:
#
#   * without `--all-targets`, integration tests under `tests/` and the bin
#     targets' unit tests are not all built, so code that only fails to compile
#     under a test cfg slips through;
#   * without `--all-features`, the optional `stripe` adapter
#     (`crates/api`, feature `stripe`) and the `web` feature of `surfaces/ui`
#     are never compiled, which is exactly where a frozen tree rots unobserved.
#
# ON THE DATABASE-DEPENDENT TESTS, AND WHY THIS GATE DOES NOT PROVIDE A DATABASE.
#
# Two suites reach for a live PostgreSQL through `FEED_RADAR_TEST_DATABASE_URL`
# and return early when it is unset: `crates/storage/tests/postgres_rls.rs` and
# the live probes in `crates/api/src/routes/articles.rs`. That convention
# predates this gate and is kept, for three reasons that are worth stating
# rather than assuming:
#
#   1. The API live probes need Redis as well, and they `expect()` on
#      `FEED_RADAR_TEST_REDIS_URL` once the database URL is set. Providing only
#      PostgreSQL would turn a clean skip into a panic — a half-provisioned
#      stack is worse than none.
#   2. `postgres_rls.rs` provisions group roles and test principals and needs
#      an owner-level connection. That is a stateful fixture, and this shell is
#      declared frozen for reference in AGENTS.md.
#   3. The security properties those migrations carry are already gated, fail
#      closed and without a live server, by `Database inspection gate`
#      (`.github/workflows/db-inspection.yml`) against
#      `db-security-manifest.json`.
#
# The cost of that choice is real and is therefore MEASURED here rather than
# left implicit. A skipped test reports `ok` like any other, so a green run says
# nothing about how much of the suite actually executed. This gate runs the
# suite with `--nocapture` so the skip lines reach the log, counts them, and
# prints the count in the verdict. A green result that says "8 skipped for want
# of a database" is honest; the same result with the skips invisible is not.
#
# The gate fails when a test fails, and when NO test executed at all — a filter
# or manifest change that empties the run would otherwise be indistinguishable
# from a clean pass.
#
# Usage: scripts/workspace-test-gate.sh
# Exit:  0 = suite green, 1 = a test failed, 2 = the gate ran nothing.

set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

# An explicit template rather than `mktemp -t NAME`: BSD/macOS appends its own
# suffix to a `-t` prefix, GNU refuses the same argument outright with "too few
# X's in template". A full path carrying the X's is accepted by both.
log="$(mktemp "${TMPDIR:-/tmp}/workspace-test-gate.XXXXXX")"
trap 'rm -f "$log"' EXIT

echo "== Workspace test suite =="
echo "   command: cargo test --workspace --all-targets --all-features -- --nocapture"
echo

# The status of `cargo test` is what this gate reports, so the run is redirected
# to a file and replayed rather than piped: a pipeline would report the status
# of the last stage. The full output is printed unconditionally below, so a
# failure is diagnosable from the job log alone.
set +e
cargo test --workspace --all-targets --all-features -- --nocapture >"$log" 2>&1
status=$?
set -e

cat "$log"

echo
echo "== Coverage accounting =="

# `grep -c` exits 1 on no match, which `set -e` would treat as fatal; every
# count below therefore carries its own `|| true`.
passed="$(awk '/^test result:/ { total += $4 } END { print total + 0 }' "$log")"
failed="$(awk '/^test result:/ { total += $6 } END { print total + 0 }' "$log")"
ignored="$(awk '/^test result:/ { total += $8 } END { print total + 0 }' "$log")"
binaries="$(grep -c '^test result:' "$log" || true)"
# Counted with `grep -o` and not `grep -c`: the harness runs tests in parallel
# and `--nocapture` lets two writes land on one line ("... okskipping live
# probe: ..."), so counting LINES silently undercounts the skips. Counting
# occurrences is the point of the number.
# The `|| true` sits INSIDE the pipeline, not after it. Under `pipefail` a
# `grep` that matches nothing exits 1 and takes the whole script down with it —
# and "matches nothing" is the expected state the day someone does provision a
# database, i.e. exactly the case this gate must survive.
skipped_db="$({ grep -o 'skipping [^:]*: FEED_RADAR_TEST_DATABASE_URL is not set' "$log" || true; } | wc -l | tr -d '[:space:]')"

echo "   test binaries executed: ${binaries}"
echo "   assertions passed:      ${passed}"
echo "   assertions failed:      ${failed}"
echo "   harness-ignored:        ${ignored}"
echo "   skipped for want of a live PostgreSQL: ${skipped_db}"

if [ "${skipped_db}" -gt 0 ]; then
  echo
  echo "   NOTE: ${skipped_db} test(s) returned early because"
  echo "         FEED_RADAR_TEST_DATABASE_URL is unset. This gate does not"
  echo "         provision PostgreSQL or Redis; see the header of this script"
  echo "         for why, and Database inspection gate for what covers the"
  echo "         migrations instead. These tests are NOT evidence in this run."
fi

echo
if [ "$status" -ne 0 ]; then
  echo "FAIL: the workspace test suite exited ${status}."
  exit 1
fi

if [ "$binaries" -eq 0 ]; then
  echo "FATAL: no test binary reported a result — the gate ran nothing." >&2
  exit 2
fi

# A suite that compiles, runs and asserts nothing is green for the wrong reason.
# `cargo test` still prints a `test result:` line per binary when a filter
# matches nothing, so the binary count above cannot catch that on its own.
if [ "$passed" -eq 0 ]; then
  echo "FATAL: ${binaries} test binaries ran and not one assertion executed." >&2
  echo "       A filter, a feature or a manifest change has emptied the suite." >&2
  exit 2
fi

echo "PASS: ${passed} assertion(s) across ${binaries} test binaries, 0 failures."
