#!/usr/bin/env bash
# Fails when an advisory waiver can outlive the reason it was granted.
#
# Two files waive RUSTSEC advisories for this tree, against two different
# graphs: `.cargo/audit.toml` covers the feature-agnostic `Cargo.lock` scan and
# `deny.toml` covers the feature-resolved `cargo deny` graph. Neither tool is a
# superset of the other, so both lists stay — and both are therefore subject to
# the same discipline. Until 2026-07-26 only one of them had any discipline at
# all: `.cargo/audit.toml` carried eight undated entries, five of which no
# longer matched anything in the graph. A waiver nobody must revisit is not a
# waiver, it is a permanent hole, and a stale one also suppresses the advisory
# it would have to report if the crate ever came back.
#
# Checks, in two tiers.
#
#   Tier 1 — decidable from tracked file content alone. These are the blocking
#   rules, and every one of them is a pure function of what is committed, so a
#   passing tree cannot turn red without a commit:
#
#     A. Every entry carries `expires=YYYY-MM-DD`, and the date is a real
#        calendar date. A waiver with no date is the defect this gate exists for.
#     B. Every entry carries `ref=`, pointing at the record that justifies it.
#        A date without a reason is a renewal with nothing to review.
#     C. `expires` is not before the `waiver-review-anchor` recorded in
#        `.cargo/audit.toml`. The anchor is the date the list was last read
#        against the resolved graph; an entry already expired at that moment is
#        a defect committed into the tree, not a clock event.
#     D. `expires` is at most HORIZON_DAYS past the anchor. Without this, the
#        gate is defeated by dating a waiver to 2099 — the renewal has to stay
#        short enough that someone actually looks again.
#     E. An advisory waived in both files carries the same date in both. This is
#        the drift that produced the original defect: `RUSTSEC-2026-0173` was
#        resolved and removed from `deny.toml` on 2026-07-12 and stayed in
#        `.cargo/audit.toml` for another fortnight.
#
#   Tier 2 — the only verdict that depends on the clock:
#
#     F. `today > expires` fails; `today` within WARN_WINDOW_DAYS of `expires`
#        warns and does not fail.
#
# On tier 2 and the surprise-red problem. A gate that silently flips to red at
# midnight on an expiry date, with no file changed, teaches people to bump the
# date rather than review the waiver — the opposite of the intent. Tier 2 is
# therefore pre-announced: for the WARN_WINDOW_DAYS before expiry every run
# prints the deadline and the remediation, so the failure is visible on every
# pull request for weeks before it blocks. It still blocks, because the brief
# for this gate is that a passed date must fail, and a warning nobody must act
# on decays into noise. The residual is deliberate and bounded: tier 1 carries
# the structural rules and never depends on the clock, and rule D bounds how far
# any renewal can push tier 2 out.
#
# Deliberately NOT implemented here: running `cargo audit` or `cargo deny`.
# Neither runs in CI in this repository, and wiring a network-fetching,
# advisory-database-dependent scan into a seconds-long manifest gate would make
# a required check fail on upstream events unrelated to the pull request — a
# newly published advisory would turn `main` red with no commit. This gate
# checks the discipline of the waiver list; verifying the list against a live
# graph stays a reviewed, local operation, recorded in
# docs/adr/0005-dependency-advisory-waivers.md.
#
# Usage: scripts/advisory-waiver-gate.sh
# Exit:  0 = clean, 1 = defect found, 2 = the gate could not examine anything.

set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

AUDIT_CONFIG=".cargo/audit.toml"
DENY_CONFIG="deny.toml"
HORIZON_DAYS=365
WARN_WINDOW_DAYS=30

# `date -d` is GNU-only and `date -v` is BSD-only, so neither can do the
# arithmetic portably. ISO 8601 dates compare correctly as strings, and the
# civil-to-day-number conversion below is pure integer awk, so the gate behaves
# identically on the macOS shell used locally and on the Ubuntu runner.
to_daynum() {
  awk -v d="$1" 'BEGIN {
    if (d !~ /^[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]$/) exit 2
    y = substr(d, 1, 4) + 0; m = substr(d, 6, 2) + 0; dd = substr(d, 9, 2) + 0
    if (m < 1 || m > 12) exit 2
    leap = (y % 4 == 0 && (y % 100 != 0 || y % 400 == 0))
    split("31,28,31,30,31,30,31,31,30,31,30,31", md, ",")
    maxd = md[m] + ((m == 2 && leap) ? 1 : 0)
    if (dd < 1 || dd > maxd) exit 2
    yy = y - (m <= 2 ? 1 : 0)
    era = int((yy >= 0 ? yy : yy - 399) / 400)
    yoe = yy - era * 400
    doy = int((153 * (m + (m > 2 ? -3 : 9)) + 2) / 5) + dd - 1
    doe = yoe * 365 + int(yoe / 4) - int(yoe / 100) + doy
    print era * 146097 + doe - 719468
  }'
}

# Emits one tab-separated record per waiver entry:
#   file <TAB> line <TAB> id <TAB> expires <TAB> ref <TAB> malformed_date_flag
# A field the entry does not carry is emitted as "-", so the caller reports the
# missing field rather than silently skipping the entry. Only the `ignore`
# array of the `[advisories]` section is read: a bans or sources list is a
# different policy with a different lifetime.
parse_entries() {
  awk '
    /^[[:space:]]*\[/ { s = $0; sub(/[[:space:]]*$/, "", s); sub(/^[[:space:]]*/, "", s); section = s }
    section == "[advisories]" && /^[[:space:]]*ignore[[:space:]]*=[[:space:]]*\[/ { in_ignore = 1; next }
    in_ignore && /^[[:space:]]*\]/ { in_ignore = 0; next }
    in_ignore {
      t = $0; sub(/^[[:space:]]+/, "", t)
      if (t == "" || t ~ /^#/) next
      id = "-"
      if (match($0, /RUSTSEC-[0-9][0-9][0-9][0-9]-[0-9][0-9][0-9][0-9]/)) id = substr($0, RSTART, RLENGTH)
      # `exp` is an awk builtin, hence `expiry` — naming it `exp` makes the
      # parser die with a syntax error rather than misparse, but it dies.
      expiry = "-"
      if (match($0, /expires=[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]/)) expiry = substr($0, RSTART + 8, 10)
      ref = "-"
      if (match($0, /ref=[^;[:space:]]+/)) ref = substr($0, RSTART + 4, RLENGTH - 4)
      bad = ($0 ~ /expires=/ && expiry == "-") ? 1 : 0
      printf "%s\t%d\t%s\t%s\t%s\t%d\n", FILENAME, FNR, id, expiry, ref, bad
    }
  ' "$1"
}

echo "== Advisory waiver discipline =="

configs=""
for f in "$AUDIT_CONFIG" "$DENY_CONFIG"; do
  if [ -f "$f" ]; then
    configs="${configs}${f}"$'\n'
  else
    echo "   note: ${f} is absent, nothing to read there"
  fi
done
config_count="$(printf '%s' "$configs" | grep -c . || true)"
echo "   waiver files found: ${config_count}"
if [ "$config_count" -eq 0 ]; then
  echo "FATAL: no waiver configuration found — the gate is not looking at anything." >&2
  exit 2
fi

# An `ignore` array the parser cannot locate is the blind spot this exit code
# exists for: the file was restructured and every entry inside it would be
# waved through as "no entries". An array that is present and empty is a
# different thing entirely — it is the goal state — so it passes.
arrays=0
while IFS= read -r f; do
  [ -n "$f" ] || continue
  if grep -qE '^[[:space:]]*ignore[[:space:]]*=[[:space:]]*\[' "$f"; then
    arrays=$((arrays + 1))
  else
    echo "   note: ${f} declares no advisory ignore array"
  fi
done <<<"$configs"
echo "   advisory ignore arrays located: ${arrays}"
if [ "$arrays" -eq 0 ]; then
  echo "FATAL: no advisory ignore array located — the gate is not looking at anything." >&2
  exit 2
fi

anchor="$(sed -n 's/^#[[:space:]]*waiver-review-anchor:[[:space:]]*\([0-9-]*\).*/\1/p' "$AUDIT_CONFIG" | head -1)"
failures=0
warnings=0

if [ -z "$anchor" ]; then
  echo "NO-ANCHOR: ${AUDIT_CONFIG} carries no \`# waiver-review-anchor: YYYY-MM-DD\` line"
  echo "         the horizon and lapsed-review rules cannot be evaluated without it"
  failures=$((failures + 1))
  anchor_num=""
else
  if ! anchor_num="$(to_daynum "$anchor")"; then
    echo "MALFORMED: waiver-review-anchor '${anchor}' in ${AUDIT_CONFIG} is not a real calendar date"
    failures=$((failures + 1))
    anchor_num=""
  else
    echo "   review anchor: ${anchor}"
  fi
fi

today="$(date -u +%Y-%m-%d)"
today_num="$(to_daynum "$today")"
echo "   today (UTC):   ${today}"

records=""
while IFS= read -r f; do
  [ -n "$f" ] || continue
  records="${records}$(parse_entries "$f")"$'\n'
done <<<"$configs"

entry_count="$(printf '%s' "$records" | grep -c . || true)"
echo "   waiver entries examined: ${entry_count}"

if [ "$entry_count" -eq 0 ]; then
  echo
  echo "PASS: every advisory ignore array is empty — no waiver is in force."
  exit 0
fi

# --- Tier 1: rules decidable from file content ------------------------------
while IFS=$'\t' read -r file line id expires ref bad; do
  [ -n "${file:-}" ] || continue
  where="${file}:${line}"

  if [ "$id" = "-" ]; then
    echo "UNPARSED: ${where} sits in an advisory ignore array but names no RUSTSEC id"
    failures=$((failures + 1))
    continue
  fi

  if [ "$expires" = "-" ]; then
    if [ "$bad" -eq 1 ]; then
      echo "MALFORMED: ${where} ${id} carries an \`expires=\` that is not YYYY-MM-DD"
    else
      echo "UNDATED: ${where} ${id} carries no \`expires=YYYY-MM-DD\`"
      echo "         an undated waiver is permanent; date it or remove it"
    fi
    failures=$((failures + 1))
    continue
  fi

  if ! exp_num="$(to_daynum "$expires")"; then
    echo "MALFORMED: ${where} ${id} expires=${expires} is not a real calendar date"
    failures=$((failures + 1))
    continue
  fi

  if [ "$ref" = "-" ]; then
    echo "UNREFERENCED: ${where} ${id} carries no \`ref=\`"
    echo "              a date with no record to review is a renewal with nothing to read"
    failures=$((failures + 1))
  fi

  if [ -n "$anchor_num" ]; then
    if [ "$exp_num" -lt "$anchor_num" ]; then
      echo "LAPSED: ${where} ${id} expires=${expires} predates the review anchor ${anchor}"
      echo "        it was already expired when the list was last reviewed"
      failures=$((failures + 1))
    elif [ "$((exp_num - anchor_num))" -gt "$HORIZON_DAYS" ]; then
      echo "OVER-HORIZON: ${where} ${id} expires=${expires} is more than ${HORIZON_DAYS} days past the anchor ${anchor}"
      echo "              a waiver dated that far out is never reviewed again"
      failures=$((failures + 1))
    fi
  fi

  # --- Tier 2: the clock-dependent verdict ----------------------------------
  if [ "$today_num" -gt "$exp_num" ]; then
    echo "EXPIRED: ${where} ${id} expired on ${expires}"
    echo "         re-verify the advisory against the graph, then remove or renew the entry"
    failures=$((failures + 1))
  elif [ "$((exp_num - today_num))" -le "$WARN_WINDOW_DAYS" ]; then
    echo "WARN: ${where} ${id} expires on ${expires}, in $((exp_num - today_num)) day(s)"
    echo "      re-verify it now; this becomes a hard failure the day after"
    warnings=$((warnings + 1))
  fi
done <<<"$records"

# --- Tier 1 rule E: cross-file date coherence -------------------------------
echo "== Cross-file coherence =="
# Entries with no parseable date are excluded here: rule A already reports them,
# and comparing a missing date against a real one would report the same defect
# twice under a label that does not describe it.
dupes="$(printf '%s\n' "$records" | awk -F'\t' 'NF >= 4 && $3 != "-" && $4 != "-" { print $3 }' | sort | uniq -d || true)"
dupe_count="$(printf '%s' "$dupes" | grep -c . || true)"
echo "   advisories waived in more than one file: ${dupe_count}"

if [ -n "$dupes" ]; then
  while IFS= read -r id; do
    [ -n "$id" ] || continue
    dates="$(printf '%s\n' "$records" | awk -F'\t' -v w="$id" 'NF >= 4 && $3 == w && $4 != "-" { print $4 }' | sort -u)"
    if [ "$(printf '%s' "$dates" | grep -c . || true)" -gt 1 ]; then
      echo "INCOHERENT: ${id} is waived with different dates: $(printf '%s' "$dates" | tr '\n' ' ')"
      echo "            the same advisory must expire on the same day in both files"
      failures=$((failures + 1))
    fi
  done <<<"$dupes"
fi

# --- Verdict ----------------------------------------------------------------
echo
if [ "$failures" -gt 0 ]; then
  echo "FAIL: ${failures} advisory waiver defect(s) found."
  exit 1
fi
if [ "$warnings" -gt 0 ]; then
  echo "PASS with ${warnings} warning(s): every waiver is dated and referenced, some are close to expiry."
  exit 0
fi
echo "PASS: every advisory waiver is dated, referenced, within horizon and coherent across files."
