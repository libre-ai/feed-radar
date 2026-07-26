# ADR 0005 — Temporary dependency advisory waivers

## Status

Accepted temporary waiver. Expires: **2026-09-30**. Remaining waivers: async-stripe only.

Resolution update (2026-07-12): `validator_derive 0.20.1` replaces `proc-macro-error2` with maintained `proc-macro-error3`; the `RUSTSEC-2026-0173` waiver is removed. The original decision table remains as the historical acceptance record.
Resolution update (2026-07-14): `scraper 0.27.0` no longer depends on `fxhash`; the `RUSTSEC-2025-0057` waiver is removed after PR #79. `cargo tree -i fxhash` returns no package, and `cargo deny` / `cargo audit` are green.
Note (2026-07-26): selecting a crypto provider for `jsonwebtoken` 10.x could have added `RUSTSEC-2023-0071` (`rsa`, Marvin) to this list. It did not: `aws_lc_rs` was retained over `rust_crypto`, and **no waiver was needed**. The arbitration is recorded below so a future review does not re-derive it.

Resolution update (2026-07-26): `.cargo/audit.toml` was reviewed entry by entry against the resolved graph for the first time since it was written. Five of its eight waivers matched nothing and were removed; the three that remain are dated `2026-09-30` and referenced here. The list had drifted because nothing forced anyone to re-read it — `RUSTSEC-2026-0173` had already been removed from `deny.toml` on 2026-07-12 and stayed in `.cargo/audit.toml` regardless. `scripts/advisory-waiver-gate.sh` now closes that: it runs in the required `No unreachable workspace code` check and fails on an undated, unreferenced, over-horizon, incoherent or expired waiver. See **Waiver review 2026-07-26** below.

## Context

`cargo deny check advisories` reports advisories through transitive dependencies:

| Advisory            | Path                                             | Reason for temporary waiver                                                                                   | Current impact                                                        | Removal plan                                                                                                    |
| ------------------- | ------------------------------------------------ | ------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------- |
| `RUSTSEC-2025-0057` | `fxhash` via `scraper`                           | Resolved by upgrading `scraper 0.27.0` in PR #79; no `fxhash` remains in the graph.                           | No current impact; `cargo tree -i fxhash` returns no package.         | Completed via the `scraper 0.27.0` upgrade and waiver removal in PR #79.                                        |
| `RUSTSEC-2026-0174` | `http-types` via optional `async-stripe` feature | Stripe is now isolated as an optional adapter, but all-features supply-chain audit still sees the dependency. | Payment adapter is optional and not part of the no-secret quickstart. | Replace/remove `async-stripe` or move to a safer payment adapter before waiver expiry (I4 follow-up).           |
| `RUSTSEC-2024-0384` | `instant` via optional `async-stripe` path       | Same optional Stripe adapter path.                                                                            | Same optional Stripe path.                                            | Replace/remove `async-stripe` or move to a safer payment adapter before waiver expiry (I4 follow-up).           |
| `RUSTSEC-2026-0173` | `proc-macro-error2` via UI/validator deps        | Needs dependency upgrade/replacement evaluation.                                                              | Build-time/proc-macro path; no scale-ready claim.                     | Upgrade or replace affected dependencies (I6).                                                                  |
| `RUSTSEC-2026-0194` | `quick-xml` NsReader via `feed-rs 2.3`           | Ingestion dependency; feed-rs upstream fix pending. No published patch as of 2026-07-03.                      | Article ingestion path; no multi-tenant trust boundary crossed.       | Resolve via upstream feed-rs patch (Option A) or force safe quick-xml constraint (Option B) by 2026-09-30 (I7). |
| `RUSTSEC-2026-0195` | `quick-xml` general via `feed-rs 2.3`            | Same ingestion path; same feed-rs upstream constraint.                                                        | Same as 2026-0194; related to the same transitive chain.              | Resolve via upstream feed-rs patch (Option A) or force safe quick-xml constraint (Option B) by 2026-09-30 (I7). |

## Waiver review 2026-07-26

### Why the two waiver files both stay

`deny.toml` and `.cargo/audit.toml` waive advisories for the same tree, and it is tempting to treat one as redundant. They do not see the same graph, and neither is a superset of the other:

| Invocation                                   | Graph read                       | Advisories reported on this tree                              |
| -------------------------------------------- | -------------------------------- | ------------------------------------------------------------- |
| `cargo audit`                                | `Cargo.lock`, feature-agnostic   | `RUSTSEC-2026-0174`, `RUSTSEC-2024-0384`, `RUSTSEC-2026-0097` |
| `cargo deny --all-features check advisories` | resolved graph, all features     | `RUSTSEC-2026-0174`, `RUSTSEC-2024-0384`                      |
| `cargo deny check advisories` (default)      | resolved graph, default features | none                                                          |

Two consequences, both measured with the ignore lists emptied:

- **`cargo deny` alone would miss `RUSTSEC-2026-0097`** at any feature set. The advisory is classed `informational = "unsound"`, which cargo-deny does not report here; `cargo audit` does. Dropping `.cargo/audit.toml` would silently drop that advisory class.
- **`cargo deny`'s two entries are inert by default and load-bearing under `--all-features`.** `cargo deny check` reports both as `advisory-not-detected` because the optional `stripe` feature is off, yet emptying the list and re-running with `--all-features` fails on exactly those two ids. They are kept for that invocation rather than deleted on the strength of the default-feature run.

Both files therefore stay, and both are now held to the same discipline. Neither tool runs in CI in this repository — that is unchanged and deliberate, see the gate note below.

### Entry-by-entry verdict on `.cargo/audit.toml`

Method: `cargo audit` run against the same `Cargo.lock` from a directory carrying no `audit.toml`, so no entry is filtered; then `cargo tree --edges normal -i <crate>` per incriminated crate; then the advisory file in the RustSec database for the patched-version range.

| Advisory            | Crate               | In `Cargo.lock` | In `--edges normal` graph        | Fires unfiltered | Verdict                                                                             |
| ------------------- | ------------------- | --------------- | -------------------------------- | ---------------- | ----------------------------------------------------------------------------------- |
| `RUSTSEC-2026-0174` | `http-types 2.12.0` | yes             | only with the `stripe` feature   | yes              | **Kept**, dated 2026-09-30                                                          |
| `RUSTSEC-2024-0384` | `instant 0.1.13`    | yes             | only with `stripe`, target-gated | yes              | **Kept**, dated 2026-09-30                                                          |
| `RUSTSEC-2026-0097` | `rand 0.7.3`        | yes             | only with the `stripe` feature   | yes              | **Kept**, dated 2026-09-30 — sole cover, cargo-deny never reports it                |
| `RUSTSEC-2024-0436` | `paste`             | **absent**      | absent                           | no               | **Removed** — obsolete                                                              |
| `RUSTSEC-2026-0173` | `proc-macro-error2` | **absent**      | absent                           | no               | **Removed** — resolved 2026-07-12, waiver survived only here                        |
| `RUSTSEC-2023-0071` | `rsa`               | **absent**      | absent                           | no               | **Removed** — ghost, see below                                                      |
| `RUSTSEC-2026-0194` | `quick-xml 0.41.0`  | yes             | yes, via `feed-rs 2.4.0`         | no               | **Removed** — advisory is `patched = [">= 0.41.0"]`, the graph is already on 0.41.0 |
| `RUSTSEC-2026-0195` | `quick-xml 0.41.0`  | yes             | yes, via `feed-rs 2.4.0`         | no               | **Removed** — same, same patched range                                              |

Notes that matter for a later review:

- **`RUSTSEC-2023-0071` was a ghost, and its stated justification was wrong twice.** It was attributed to "rsa via sqlx-mysql". `rsa` has no `[[package]]` entry in `Cargo.lock` at all and `cargo tree --edges normal -i rsa` reports `package ID specification 'rsa' did not match any packages`. `sqlx-mysql` _is_ in the lock at 0.9.0 — the premise that it had left the tree is false — but `sqlx-mysql 0.9.0` declares no `rsa` dependency, so the named edge does not exist either. The advisory has `patched = []`, so any `rsa` in the graph would trigger it: this entry would have suppressed the advisory silently had `rust_crypto` been selected for `jsonwebtoken`, which is precisely the decision taken a few hours earlier in this same ADR. That is the concrete cost of an unreviewed waiver, and the reason for the gate.
- **The two `quick-xml` entries were removed for a different reason than the other three.** The crate is still in the production graph; it is the _version_ that moved past the fix. The comment claimed "feed-rs 2.3 pins quick-xml 0.37"; the tree carries `feed-rs 2.4.0` and `quick-xml 0.41.0`, and both advisories are patched at `>= 0.41.0`. Removing the waivers is what makes a future downgrade audible again — kept, they would have muted it.
- Removing an entry that matches nothing costs no coverage and restores a signal. Every removal above is a crate absent from the graph or an advisory whose patched range the graph already satisfies, so no `cargo audit` or `cargo deny` verdict changes: both are green before and after.

### The gate, and what it deliberately does not do

`scripts/advisory-waiver-gate.sh` runs in the `Dead code` workflow, whose job name `No unreachable workspace code` is an existing required check. It was folded into that job rather than given a workflow of its own for the reason already recorded there: an unrequired check is decorative.

It splits its rules in two tiers. Tier 1 — a date is present and real, a `ref=` is present, the date is not before the `waiver-review-anchor`, not more than 365 days past it, and identical for an advisory waived in both files — is a pure function of tracked content, so a green tree cannot turn red without a commit. Tier 2 is the single clock-dependent verdict: a passed expiry fails, and the 30 days before it warn on every run without failing, so the block is announced weeks ahead rather than arriving as a surprise red on a morning when nothing changed. The horizon rule bounds how far a renewal can push that deadline out, which is what stops "re-date it to 2099" from being the path of least resistance.

The gate does **not** run `cargo audit` or `cargo deny`. Both fetch an advisory database, so wiring them into a required, seconds-long manifest check would let a newly published upstream advisory turn `main` red with no commit in this repository — the same surprise-red failure mode, sourced externally. Verifying the list against a live graph stays a reviewed operation, and its result is recorded in this ADR.

## JWT crypto provider — arbitration (no waiver required)

`jsonwebtoken` 10.x ships no crypto backend and panics on the first sign or verify unless exactly one provider feature is enabled. Choosing one was unavoidable; choosing badly would have added an advisory. Recorded here because the two candidates differ in supply-chain cost, not in functionality.

| Candidate                  | New crates pulled                                                              | New advisory                                                      | Already in this graph                                                       |
| -------------------------- | ------------------------------------------------------------------------------ | ----------------------------------------------------------------- | --------------------------------------------------------------------------- |
| `aws_lc_rs` **(retained)** | none                                                                           | none                                                              | yes — `aws-lc-rs 1.17.1` via `rustls`, itself via `reqwest` and `sqlx-core` |
| `rust_crypto`              | `rsa`, `ed25519-dalek`, `p256`, `p384`, `hmac`, `num-bigint-dig`, ~28 in total | `RUSTSEC-2023-0071` (`rsa`, Marvin), **no upstream patch exists** | no                                                                          |

**Decision: `aws_lc_rs`.** It is already compiled for the TLS stack, so enabling it on `jsonwebtoken` costs nothing in the dependency graph and introduces no advisory. `rust_crypto` was evaluated first and would have required a permanent-in-practice waiver for `RUSTSEC-2023-0071`: the advisory has no fix ("No safe upgrade is available"), so the waiver would have had a review date but no resolution path.

Two points that survive the choice and are worth keeping:

- **The providers are mutually exclusive.** The crate matches on `all(feature = "rust_crypto", not(feature = "aws_lc_rs"))` and its mirror, so enabling **both** falls through to exactly the same panic as enabling neither. `scripts/jwt-crypto-provider-gate.sh` fails on zero _and_ on more than one.
- **The feature belongs on the workspace dependency**, not on a member, so no member can inherit a provider-less `jsonwebtoken`. It must not sit in `[dev-dependencies]`: under resolver "2" those features never reach `cargo build`, which yields green tests over a server that still panics.

Had `rust_crypto` been retained, the waiver would have rested on the vulnerable path being linked but unreachable — tokens are `HS256` only, `decode` refuses a foreign `alg` at `decoding.rs:283` before building a verifier on line 287, and no RSA key material exists (keys are `from_secret`, an HMAC secret). That argument was sound but unnecessary: not linking `rsa` at all is strictly stronger than arguing it is unreachable. **Revisit only if a future change needs an algorithm `aws-lc-rs` does not provide**; re-opening this ADR is then required, since the comparison above would no longer hold.

## Decision

The advisories are temporarily ignored in `deny.toml` to unblock readiness planning, not product expansion.

This waiver does **not** authorize:

- new product UI expansion;
- mandatory Stripe dependency;
- using affected paths for sensitive provider/BYOK material without tests;
- implementation planning without the harness gates.

## Required follow-up before expiry

1. Replace/remove the optional `async-stripe` adapter or move to a safer payment adapter (covers RUSTSEC-2026-0174, 2024-0384, 2026-0097). Default/core build isolation is complete, but all-features audit still requires the waiver.
2. ~~Evaluate replacing `scraper` or its affected transitive path (RUSTSEC-2025-0057).~~ Resolved by upgrading `scraper 0.27.0` in PR #79; `cargo tree -i fxhash` returns no package.
3. ~~Upgrade or replace UI/validator dependencies pulling unmaintained proc-macro crates (RUSTSEC-2026-0173).~~ Resolved by `validator_derive 0.20.1` on 2026-07-12.
4. ~~Resolve `quick-xml` advisories via upstream patch or safe version constraint (RUSTSEC-2026-0194, 2026-0195); deadline 2026-09-30.~~ Resolved upstream: `feed-rs 2.4.0` carries `quick-xml 0.41.0` and both advisories are patched at `>= 0.41.0`. Waivers removed 2026-07-26.
5. Remove advisory ignores when fixed — now enforced rather than remembered, by `scripts/advisory-waiver-gate.sh` in the required `No unreachable workspace code` check.

## Acceptance impact

With this ADR and `deny.toml`, advisory risk is explicit and time-bounded. FeedMind may be considered ready for planning-only harness packaging only if all other gates pass and the proof records the waiver reference.
