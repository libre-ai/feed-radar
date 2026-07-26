#!/usr/bin/env bash
# Fails when unreachable code reappears in the Rust workspace.
#
# Three deterministic checks, none of which compiles the workspace:
#
#   A. Orphan workspace members  — a member that produces no binary and that no
#      other member depends on is unreachable. This is the check that catches a
#      compatibility facade left behind after its consumers migrated away.
#   B. Orphan workspace dependencies — a `[workspace.dependencies]` key that no
#      member inherits with `workspace = true` is a declaration reaching nothing.
#   C. Unreachable module files — a `.rs` file under a member's `src/` that no
#      `mod` declaration in that crate names is compiled by nothing.
#
# Deliberately NOT implemented here: a gate on rustc's `unused_crate_dependencies`
# lint. That lint reports per compilation target, so a dependency used only by
# `#[cfg(test)]` code or only by an integration test is reported as unused for
# the plain lib/bin target. It is a useful audit instrument, not a gate — it
# cannot distinguish those cases from genuinely dead declarations, and it is
# blind to dependencies that exist purely to activate a Cargo feature on a
# transitive crate (see `password-hash` in crates/api and crates/cli).
#
# Usage: scripts/dead-code-gate.sh
# Exit:  0 = clean, 1 = dead code found, 2 = the gate could not examine anything.

set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

command -v cargo >/dev/null || { echo "FATAL: cargo is required" >&2; exit 2; }
command -v jq >/dev/null || { echo "FATAL: jq is required" >&2; exit 2; }

metadata="$(cargo metadata --no-deps --format-version 1 --offline 2>/dev/null \
  || cargo metadata --no-deps --format-version 1)"

failures=0

# --- Check A: orphan workspace members -------------------------------------
# A member is reachable when it exposes a binary target or when another member
# names it as a dependency (normal, dev or build).
members="$(jq -r '.packages[].name' <<<"$metadata" | sort -u)"
member_count="$(printf '%s\n' "$members" | grep -c . || true)"

echo "== Check A: workspace members reachable =="
echo "   members examined: ${member_count}"
if [ "$member_count" -eq 0 ]; then
  echo "FATAL: no workspace member examined — the gate is not looking at anything." >&2
  exit 2
fi

with_bin="$(jq -r '.packages[] | select([.targets[].kind[]] | index("bin")) | .name' <<<"$metadata" | sort -u)"
depended_on="$(jq -r '.packages[].dependencies[].name' <<<"$metadata" | sort -u)"

while IFS= read -r member; do
  [ -n "$member" ] || continue
  if grep -qxF "$member" <<<"$with_bin"; then continue; fi
  if grep -qxF "$member" <<<"$depended_on"; then continue; fi
  manifest="$(jq -r --arg m "$member" '.packages[] | select(.name==$m) | .manifest_path' <<<"$metadata")"
  echo "DEAD: workspace member '${member}' has no binary target and no member depends on it"
  echo "      ${manifest#"$PWD"/}"
  failures=$((failures + 1))
done <<<"$members"

# --- Check B: orphan [workspace.dependencies] entries -----------------------
# Keys declared at the workspace root that no member inherits.
ws_keys="$(awk '
  /^\[workspace\.dependencies\]/ { inside = 1; next }
  /^\[/ { inside = 0 }
  inside && /^[A-Za-z0-9_-]+[[:space:]]*=/ { sub(/[[:space:]]*=.*/, ""); print }
' Cargo.toml | sort -u)"
ws_key_count="$(printf '%s\n' "$ws_keys" | grep -c . || true)"

echo "== Check B: workspace dependencies inherited =="
echo "   entries examined: ${ws_key_count}"
if [ "$ws_key_count" -eq 0 ]; then
  echo "FATAL: no [workspace.dependencies] entry examined — the gate is not looking at anything." >&2
  exit 2
fi

member_manifests="$(jq -r '.packages[].manifest_path' <<<"$metadata")"
while IFS= read -r key; do
  [ -n "$key" ] || continue
  found=0
  while IFS= read -r manifest; do
    [ -n "$manifest" ] || continue
    if grep -qE "^[[:space:]]*${key}[[:space:]]*=[[:space:]]*\{[^}]*workspace[[:space:]]*=[[:space:]]*true" "$manifest"; then
      found=1
      break
    fi
  done <<<"$member_manifests"
  if [ "$found" -eq 0 ]; then
    echo "DEAD: [workspace.dependencies] entry '${key}' is inherited by no member"
    failures=$((failures + 1))
  fi
done <<<"$ws_keys"

# --- Check C: unreachable module files --------------------------------------
# Every .rs file under a member's src/ must be named by a `mod` declaration in
# its own crate, unless it is a crate root (lib.rs / main.rs) or a bin target
# root declared in the manifest.
echo "== Check C: module files reachable =="
module_count=0
while IFS= read -r manifest; do
  [ -n "$manifest" ] || continue
  crate_dir="$(dirname "$manifest")"
  [ -d "${crate_dir}/src" ] || continue

  # Crate roots and manifest-declared target paths are reachable by definition.
  target_roots="$(jq -r --arg mp "$manifest" \
    '.packages[] | select(.manifest_path==$mp) | .targets[].src_path' <<<"$metadata")"

  mod_decls="$(grep -rhoE '^[[:space:]]*(pub[[:space:]]+)?mod[[:space:]]+[A-Za-z0-9_]+[[:space:]]*;' \
    "${crate_dir}/src" 2>/dev/null | grep -oE '[A-Za-z0-9_]+[[:space:]]*;$' | tr -d ' ;' | sort -u || true)"

  while IFS= read -r rs; do
    [ -n "$rs" ] || continue
    module_count=$((module_count + 1))
    if grep -qxF "$rs" <<<"$target_roots"; then continue; fi
    base="$(basename "$rs" .rs)"
    if [ "$base" = "mod" ]; then
      base="$(basename "$(dirname "$rs")")"
    fi
    if ! grep -qxF "$base" <<<"$mod_decls"; then
      echo "DEAD: ${rs#"$PWD"/} is named by no \`mod\` declaration in its crate"
      failures=$((failures + 1))
    fi
  done <<<"$(find "${crate_dir}/src" -name '*.rs' -type f | sort)"
done <<<"$member_manifests"

echo "   module files examined: ${module_count}"
if [ "$module_count" -eq 0 ]; then
  echo "FATAL: no module file examined — the gate is not looking at anything." >&2
  exit 2
fi

# --- Verdict ----------------------------------------------------------------
echo
if [ "$failures" -gt 0 ]; then
  echo "FAIL: ${failures} unreachable item(s) found."
  exit 1
fi
echo "PASS: no unreachable workspace member, dependency declaration or module file."
