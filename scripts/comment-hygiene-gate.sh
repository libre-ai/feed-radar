#!/usr/bin/env bash
# Fails when a comment carries something the compiler can never check.
#
# Two deterministic checks over tracked Rust sources. Both target defects that
# are decidable from syntax alone — the rest of comment quality (does this
# explain *why*?) is a judgement call and is deliberately NOT gated here.
#
#   A. Commented-out code — a statement parked behind `//`. It is unreachable,
#      untested and unversioned in any meaningful sense; git history already
#      keeps it. The match requires BOTH a code-shaped opening token AND a
#      code-shaped terminator (`;` `{` `}`), because either signal alone is a
#      false-positive machine: prose legitimately begins with "use" or "return",
#      and prose legitimately contains `();` when it names a function.
#   B. Unanchored TODO/FIXME/HACK/XXX — a marker with no scope, no issue, no
#      spec reference is a note to a person who has since left the file. An
#      anchored one is a plan. The accepted anchors are the `TODO(scope)` form
#      already used in this workspace, an issue number, a URL, or an uppercase
#      document id such as `AMD-003`, `ADR-0005` or `RUSTSEC-2025-0057`.
#
# Deliberately NOT implemented here: a duplicate-comment check. On this corpus
# it is dominated by `// =====` navigation banners, which are house style rather
# than a defect, and the genuine remainder (`// Log event` four times in one
# service) is paraphrase — removing it is an editorial call, not a mechanical
# one. Gating it would either sit red on main or force cosmetic churn through
# a workspace that AGENTS.md freezes for reference.
#
# Equally NOT implemented: any comment-to-code ratio. Such a metric rewards
# writing comments rather than writing useful ones.
#
# Usage: scripts/comment-hygiene-gate.sh
# Exit:  0 = clean, 1 = defect found, 2 = the gate could not examine anything.

set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

# `grep -P` does not exist on BSD/macOS: it exits with a usage message that a
# caller easily misreads as "no match". Everything below is POSIX ERE with
# explicit character classes, and every grep return code is handled.
files="$(git ls-files -- '*.rs')"
file_count="$(printf '%s\n' "$files" | grep -c . || true)"

echo "== Comment hygiene =="
echo "   Rust files scanned: ${file_count}"
if [ "$file_count" -eq 0 ]; then
  echo "FATAL: no Rust file examined — the gate is not looking at anything." >&2
  exit 2
fi

# --- Check A: commented-out code --------------------------------------------
# Doc comments (`///`, `//!`) are public documentation and are excluded: the
# leading `[^/!]` after the opener rejects them. Lines carrying a URL are
# excluded so a cited link never reads as code.
#
# Every exclusion below is applied to the COMMENT BODY, never to the raw
# `git grep` line. `git grep -n` prefixes each hit with `path:line:`, so a
# commented line renders as `probe.rs:3://  let x = 1;` — which contains the
# substring `://` produced by the line-number colon meeting the comment slashes.
# Filtering the raw line with `grep -v '://'` therefore discards EVERY hit and
# the check reports clean while seeing nothing. Strip the prefix first.
opener='(let|fn|use|impl|struct|enum|mod|pub|match|if|for|while|return|async|const|static|unsafe|trait|type|loop|else)'
code_like="^[[:space:]]*//[[:space:]]*${opener}[[:space:]]+.*[;{}][[:space:]]*$"
brace_like='^[[:space:]]*//[[:space:]]*[})][;,]?[[:space:]]*$'

strip_prefix='{ body = $0; sub(/^[^:]+:[0-9]+:/, "", body); }'

echo "== Check A: no commented-out code =="
hits_a="$(git grep -n -I -E -e "$code_like" -e "$brace_like" -- '*.rs' \
  | awk "${strip_prefix} body !~ /:\\/\\// && body !~ /allow-commented-code/" \
  || true)"

if [ -n "$hits_a" ]; then
  printf '%s\n' "$hits_a" | while IFS= read -r line; do
    echo "DEAD: ${line}"
  done
fi

# --- Check B: anchored markers ----------------------------------------------
# An anchor is: the TODO(scope) form, an issue number, a URL, or a document id.
echo "== Check B: markers carry an anchor =="
markers="$(git grep -n -I -E '(TODO|FIXME|HACK|XXX)' -- '*.rs' || true)"
marker_count="$(printf '%s\n' "$markers" | grep -c . || true)"
echo "   markers examined: ${marker_count}"

if [ -n "$markers" ]; then
  # Same discipline as check A: judge the comment body, never the path prefix,
  # so a directory name can never accidentally satisfy an anchor.
  unanchored="$(printf '%s\n' "$markers" \
    | awk "${strip_prefix} body ~ /(TODO|FIXME|HACK|XXX)[[:space:]]*:/ \
        && body !~ /(TODO|FIXME|HACK|XXX)\\(/ \
        && body !~ /#[0-9]+/ \
        && body !~ /http/ \
        && body !~ /[A-Z][A-Z]+-[0-9]+/" || true)"

  if [ -n "$unanchored" ]; then
    printf '%s\n' "$unanchored" | while IFS= read -r line; do
      echo "UNANCHORED: ${line}"
    done
  fi
fi

# --- Verdict ----------------------------------------------------------------
# The verdict is recomputed from the captured hit sets: the `while` loops above
# run in subshells, so a counter incremented inside them would not survive.
total=0
[ -n "$hits_a" ] && total=$((total + $(printf '%s\n' "$hits_a" | grep -c . || true)))
[ -n "${unanchored:-}" ] && total=$((total + $(printf '%s\n' "$unanchored" | grep -c . || true)))

echo
if [ "$total" -gt 0 ]; then
  echo "FAIL: ${total} comment defect(s) found."
  exit 1
fi
echo "PASS: no commented-out code, no unanchored marker."
