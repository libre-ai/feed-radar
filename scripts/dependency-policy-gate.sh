#!/usr/bin/env bash
# Enforces the DETERMINISTIC half of `deny.toml`, and states what it cannot see.
#
# `deny.toml` and `.cargo/audit.toml` were reviewed and dated on 2026-07-26, and
# until this gate landed no tool in CI read either of them. The policy was
# impeccable and unenforced.
#
# WHY THIS GATE RUNS THREE CHECKS AND NOT FOUR.
#
# `cargo deny check` has four sections, and they do not have the same failure
# mode:
#
#   bans, licenses, sources — a pure function of the committed tree. They read
#     `Cargo.lock`, the workspace manifests and the licence metadata of already
#     published crate versions, all of which are immutable once published. Two
#     runs a month apart on the same commit give the same verdict.
#
#   advisories — a function of the tree AND of the RustSec advisory database,
#     which is fetched at run time, plus the crates.io yank flags, which upstream
#     maintainers can flip at any moment. A commit that passed yesterday fails
#     today because someone else published an advisory.
#
# Only the first three are enforced here, and only they belong in a required
# check. `scripts/advisory-waiver-gate.sh` already refused to invoke a
# network-fetching scan for this exact reason; this gate keeps that boundary
# instead of quietly crossing it. A required check that turns `main` red with no
# commit teaches people to ignore or bypass it, which costs more than the check
# was worth.
#
# The advisory half is not abandoned — it runs in the separate, deliberately
# NON-REQUIRED `Advisory report` job, where a red mark is a signal to read and
# not a merge blocker.
#
# WHAT THIS GATE DOES NOT SEE. Two blind spots, measured rather than assumed:
#
#   1. Yanked crates. `yanked` lives under `[advisories]`, so it is out of scope
#      here by construction. It is also volatile: any version can be yanked
#      upstream at any time.
#   2. Anything cargo-deny filters out of the graph. cargo-deny resolves a
#      feature- and target-selected graph and drops what it cannot reach — 42
#      crates on this tree under default features. `spin 0.9.8` is one of them,
#      and it is genuinely yanked upstream: `cargo deny` reports `advisories ok`
#      on it even with `yanked = "deny"`, not because yank detection is broken
#      (it is not — it fires correctly on a reachable yanked crate) but because
#      the crate is not in the graph it examines. `cargo audit` reads
#      `Cargo.lock` feature-agnostically and does see it. That is the concrete
#      instance of the rule recorded in `.cargo/audit.toml`: neither tool is a
#      superset of the other, so both configuration files stay.
#
# Usage: scripts/dependency-policy-gate.sh
# Exit:  0 = policy holds, 1 = a violation, 2 = the gate could not run.

set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

# The version cargo-deny's verdicts were established against. The workflow pins
# the binary by SHA-256, so this is a local-run advisory rather than a second
# enforcement point: a different version is allowed to disagree, and the reader
# should know which one produced the result.
PINNED_VERSION="0.19.5"

echo "== Dependency policy (deterministic checks) =="

if ! command -v cargo-deny >/dev/null 2>&1; then
  echo "FATAL: cargo-deny is not on PATH — the gate cannot examine anything." >&2
  exit 2
fi

if [ ! -f deny.toml ]; then
  echo "FATAL: deny.toml is absent — the gate has no policy to enforce." >&2
  exit 2
fi

running_version="$(cargo deny --version 2>/dev/null | awk '{print $2}')"
echo "   cargo-deny version: ${running_version:-unknown} (pinned in CI: ${PINNED_VERSION})"
if [ -n "${running_version:-}" ] && [ "${running_version}" != "${PINNED_VERSION}" ]; then
  echo "   note: this is not the pinned version; a differing verdict is not"
  echo "         necessarily a change in the tree."
fi

echo "   checks: bans, licenses, sources"
echo "   excluded: advisories (volatile — see the header and the Advisory report job)"
echo

# `--locked` asserts Cargo.lock is used as committed and not silently updated;
# `--disable-fetch` forbids reaching for the advisory database. Together they
# make the "cannot go red without a commit" property structural rather than
# incidental to which sections were named.
set +e
cargo deny --locked check bans licenses sources --disable-fetch
status=$?
set -e

echo
if [ "$status" -ne 0 ]; then
  echo "FAIL: dependency policy violated (cargo-deny exited ${status})."
  echo "      A ban, a licence outside the allow-list, or an unknown source."
  exit 1
fi

echo "PASS: bans, licenses and sources hold against deny.toml."
echo "      Advisories and yank status are NOT covered by this check."
