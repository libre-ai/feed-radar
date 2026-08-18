# radar Canonical Agent Rules

## Purpose

Radar is the couche-1 product that turns unmanageable, opaque-algorithm feed
subscriptions (OPML, RSS, Atom, JSON Feed) into curated, explainable exports:
every selection traces to a readable rule, and nothing leaves the user's
machine. The authoritative statement, current situation and evidence live in
`project.v1.yaml` (never duplicated here) — doctrine and the fleet template
come from:
https://raw.githubusercontent.com/libre-ai/governance/main/docs/method/CONTEXT-TEMPLATE.md

## Domain doctrine

- The pre-big-bang Rust workspace (crates, legacy CI, `.claude/`, the prior
  `AGENTS.md`, `_bmad/`) was retired from the working tree on 2026-07-30
  (`b19e2d8`, "retire the frozen-home legacy tree") — it is **for reference
  only**, fully recoverable from git history, never a current build
  instruction. Reconstruction runs entirely through the Bun socle grafted
  from the hub (ADR-0020, γ 3.5): `apps/radar`, root `package.json`.
- `.claude/` left tracking in that same commit; there is nothing left to
  remove today — this line is the historical pointer, not a pending action.
- Contract shapes are canonical in `libre-ai/contracts`
  (https://github.com/libre-ai/contracts), consumed here as a pinned
  git-dep — never redefined locally.

## Commands

- `bun install && bun run check` — the full gate chain (bun floor, toolchain,
  `apps/radar` install + test, secret scan, personal-data boundary, lint).
- `bun test --cwd apps/radar` — the app's own test suite alone.

## Working here

- English for this file and all versioned doctrine; French stays the human
  communication language.
- Security > quality > performance > completeness.
- Never a machine-local absolute path in a tracked file.
- Stage files before running tree-walking gates; never hide a red check.
