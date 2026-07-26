# AGENTS.md

Canonical agent-context surface for this repository. `CLAUDE.md` is a minimal adapter that imports this file.

## Purpose

Radar is explainable feed selection and portable curation: subscribe to RSS, Atom and JSON Feed sources, apply visible deterministic rules to decide which items to keep, inspect rule-by-rule why each decision was made, and export a curated set with provenance. Feeds stay untrusted until the user decides they matter; selection is never an opaque ranking.

## Scope / Non-scope

- **Reserved home.** This repository is the public reserved home of Radar. The product is being rebuilt in the canonical base repository [`libre-ai/libre-ai`](https://github.com/libre-ai/libre-ai) (multi-repo topology, [ADR-0008](https://github.com/libre-ai/libre-ai/blob/main/docs/adr/0008-multi-repo-target-topology-and-brand.md)); it reopens as the real product repository when the owner activates it (wave 4).
- The legacy implementation carried here is **frozen for reference**: the Rust workspace (`crates/{crypto,domain,ingest,opml,rules,sync,storage,api,worker,cli}` and `surfaces/ui`), the PostgreSQL migrations and their security manifest, the Playwright e2e suite, and the `examples/` corpus.
- **Non-scope: new product development in this repository until activation.** Work on the parser, rule engine, contracts and product host happens in the base repository.
- `rumble-feed-mind` and the `feedmind-*` crate names are a retired brand and historical package identifiers. They survive in `Cargo.toml`, crate names and script names because renaming frozen code buys nothing — they are not the product name.

## Engineering doctrine (frozen for reference)

These rules governed the implementation carried here. They are recorded because they explain why the code has the shape it has — not as instructions to build against today.

- **Rust-first product stack** — domain, rules, sync and adapters are Rust; durable surfaces consume the design system for tokens, accessibility and UI i18n.
- **Thin adapters** — `api`, `worker`, `cli` and UI shells carry no durable business logic.
- **Explainability is mandatory** — a rule or sorting decision must produce a reason and, where possible, an evidence trail.
- **Event-minded** — prefer replayable business events (`FeedFetched`, `ArticleDiscovered`, `RuleEvaluated`) to opaque mutations.
- **Sovereignty** — self-hostable, PostgreSQL/Redis, local SQLite when offline, EU hosting target, no mandatory dependency on a US hyperscaler.
- **BYOK** — user AI keys are encrypted, never logged, never committed.
- **Evidence over promise** — every increment leaves a reproducible verification command.
- **Retired surfaces** — the legacy Next.js app and the Leptos spike were removed from the workspace; `docs/spikes/leptos-web-shell.md` keeps the evaluation as a migration reference, not a target.

## Commands

Verified against `Cargo.toml`, `e2e/package.json`, `deny.toml` and `scripts/`. Most of them now run in CI — see **CI gates**. The exceptions, run by no workflow, are `cargo fmt`, `cargo clippy` and the Playwright e2e suite.

- Rust workspace: `cargo test --workspace --all-targets --all-features` (eleven members: `crates/crypto`, `crates/domain`, `crates/ingest`, `crates/opml`, `crates/rules`, `crates/sync`, `crates/storage`, `crates/api`, `crates/worker`, `crates/cli`, `surfaces/ui`). Run in CI by `scripts/workspace-test-gate.sh`. Tests needing a live PostgreSQL return early unless `FEED_RADAR_TEST_DATABASE_URL` is set; the API live probes additionally require `FEED_RADAR_TEST_REDIS_URL` once it is.
- Format and check: `cargo fmt --all --check`, `cargo check`.
- Lint: `cargo clippy --workspace --all-targets --all-features -- -D warnings`.
- Dependency policy, deterministic half: `cargo deny check bans licenses sources` (`deny.toml`), run in CI by `scripts/dependency-policy-gate.sh`.
- Dependency policy, volatile half: `cargo deny --all-features check advisories` and `cargo audit` (`.cargo/audit.toml`). Both read databases that change without this repository changing, so they run in a non-required job.
- e2e (from `e2e/`): `npm run test` (Playwright).
- Scripts in `scripts/`: `build-feedmind-app.sh`, `verify-feedmind-app.sh`, `generate-live-radar-proof.sh`, `verify-design-system.py`, `dead-code-gate.sh`, `comment-hygiene-gate.sh`, `advisory-waiver-gate.sh`, `jwt-crypto-provider-gate.sh`, `workspace-test-gate.sh`, `dependency-policy-gate.sh`, and the PostgreSQL role fixtures under `scripts/postgres/`.

## CI gates

The legacy product CI (Rust, security, contracts, release) was retired from this reserved shell. Seven workflows remain and run on every pull request. The first four never compile the workspace:

- `Context hygiene` (`.github/workflows/context-hygiene.yml`) — blocks private identifiers and machine-local paths from entering the public tree.
- `db-inspection` (`.github/workflows/db-inspection.yml`) — fail-closed inspection of `migrations/` against `db-security-manifest.json`.
- `Licensing` (`.github/workflows/licensing.yml`) — REUSE compliance against the per-path mapping in `REUSE.toml`.
- `Dead code` (`.github/workflows/dead-code.yml`) — fails when a workspace member, a `[workspace.dependencies]` entry or a module file becomes unreachable (`scripts/dead-code-gate.sh`); the same job also fails on commented-out code and on a `TODO`/`FIXME`/`HACK` carrying no scope, issue or document reference (`scripts/comment-hygiene-gate.sh`), and on an advisory waiver in `.cargo/audit.toml` or `deny.toml` that carries no expiry date, no reference, a date beyond the review horizon, a date that disagrees between the two files, or a date already passed (`scripts/advisory-waiver-gate.sh`). The three scripts share this job so each inherits its branch-protection requirement.

The remaining three **do** compile, or read a dependency graph, deliberately:

- `JWT crypto provider` (`.github/workflows/jwt-crypto-provider.yml`), job name `JWT signs and verifies` — `jsonwebtoken` 10.x selects its crypto backend from crate features and panics at the first sign or verify when the feature set names none, or both. `cargo check`, `cargo clippy` and `cargo build` are all green on a binary that aborts on its first login. The job runs `scripts/jwt-crypto-provider-gate.sh` (graph-only: exactly one provider must reach `cargo tree --edges normal`, so a provider parked in `[dev-dependencies]` cannot satisfy it) and then the `routes::auth::tests` round trip, which signs and verifies a real token. It stays narrow on purpose and is **not** the workspace suite.
- `Workspace tests` (`.github/workflows/workspace-tests.yml`), job name `Workspace tests pass` — runs `scripts/workspace-test-gate.sh`, i.e. `cargo test --workspace --all-targets --all-features` over all eleven members. It fails on any test failure, and also when the suite executes no assertion at all. It does **not** provision PostgreSQL or Redis: the database-dependent tests return early, and the gate prints how many did so, so the green mark never implies coverage it lacks.
- `Dependency policy` (`.github/workflows/dependency-policy.yml`) — two jobs split along whether a verdict can change without a commit. `Dependency policy holds` runs `scripts/dependency-policy-gate.sh` (`cargo deny check bans licenses sources`, `--locked --disable-fetch`), a pure function of the committed tree, and is safe to require. `Advisory report` runs `cargo deny --all-features check advisories` and `cargo audit`; both read databases that upstream changes without this repository changing, so that job **must never be a required check**. Both scanners are pinned by version and SHA-256.

Each is its own workflow rather than a step folded into another: a check must be named for what it covers. `Dead code` is a seconds-long manifest gate, and a check named for unreachable code must not fail because a crate stopped compiling, a test regressed or a licence drifted.

Required checks on `main` are the five names `No private identifiers or machine-local paths`, `REUSE compliance`, `Database inspection gate`, `No unreachable workspace code` and `JWT signs and verifies`. `Workspace tests pass` and `Dependency policy holds` are intended to join them and are not enforcing until they do.

## Links

- [README](README.md) · [Français](README.fr.md)
- [docs/adr/](docs/adr/) — seven accepted ADRs, including `0002-rust-first-product-stack`, `0004-auth-boundary-jwt-session-biscuit-delegation`, `0006-tenant-context-and-row-level-security` and `0007-bounded-public-feed-sync`
- [docs/product-readiness.md](docs/product-readiness.md) — readiness cockpit for the frozen implementation
- [ROADMAP.md](ROADMAP.md) · [CONTRIBUTING.md](CONTRIBUTING.md) · [SECURITY.md](SECURITY.md)

## Modification rules

- Read the relevant files before editing.
- Prefer small, reversible changes.
- Never introduce `unwrap()` outside tests without justification.
- Never silence an error with a global `allow` without an ADR or a local comment.
- Keep `Cargo.lock` versioned for reproducibility.
- Record any structural decision in `docs/adr/`.
- Never add a major dependency without a licence, sovereignty, maintenance and rejected-alternatives justification.
