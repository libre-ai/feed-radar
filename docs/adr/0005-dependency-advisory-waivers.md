# ADR 0005 — Temporary dependency advisory waivers

## Status

Accepted temporary waiver. Expires: **2026-09-30**. Remaining waivers: async-stripe, and `rsa` via the JWT crypto provider.

Resolution update (2026-07-12): `validator_derive 0.20.1` replaces `proc-macro-error2` with maintained `proc-macro-error3`; the `RUSTSEC-2026-0173` waiver is removed. The original decision table remains as the historical acceptance record.
Resolution update (2026-07-14): `scraper 0.27.0` no longer depends on `fxhash`; the `RUSTSEC-2025-0057` waiver is removed after PR #79. `cargo tree -i fxhash` returns no package, and `cargo deny` / `cargo audit` are green.
Addition (2026-07-26): `RUSTSEC-2023-0071` (`rsa`, Marvin) enters the ignore list as a consequence of selecting the `rust_crypto` provider for `jsonwebtoken` 10.x. See the dedicated section below — this one is waived as _linked but unreachable_, not as _accepted exposure_.

## Context

`cargo deny check advisories` reports advisories through transitive dependencies:

| Advisory            | Path                                             | Reason for temporary waiver                                                                                                                                          | Current impact                                                                                                                        | Removal plan                                                                                                                                                             |
| ------------------- | ------------------------------------------------ | -------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `RUSTSEC-2025-0057` | `fxhash` via `scraper`                           | Resolved by upgrading `scraper 0.27.0` in PR #79; no `fxhash` remains in the graph.                                                                                  | No current impact; `cargo tree -i fxhash` returns no package.                                                                         | Completed via the `scraper 0.27.0` upgrade and waiver removal in PR #79.                                                                                                 |
| `RUSTSEC-2026-0174` | `http-types` via optional `async-stripe` feature | Stripe is now isolated as an optional adapter, but all-features supply-chain audit still sees the dependency.                                                        | Payment adapter is optional and not part of the no-secret quickstart.                                                                 | Replace/remove `async-stripe` or move to a safer payment adapter before waiver expiry (I4 follow-up).                                                                    |
| `RUSTSEC-2024-0384` | `instant` via optional `async-stripe` path       | Same optional Stripe adapter path.                                                                                                                                   | Same optional Stripe path.                                                                                                            | Replace/remove `async-stripe` or move to a safer payment adapter before waiver expiry (I4 follow-up).                                                                    |
| `RUSTSEC-2026-0173` | `proc-macro-error2` via UI/validator deps        | Needs dependency upgrade/replacement evaluation.                                                                                                                     | Build-time/proc-macro path; no scale-ready claim.                                                                                     | Upgrade or replace affected dependencies (I6).                                                                                                                           |
| `RUSTSEC-2026-0194` | `quick-xml` NsReader via `feed-rs 2.3`           | Ingestion dependency; feed-rs upstream fix pending. No published patch as of 2026-07-03.                                                                             | Article ingestion path; no multi-tenant trust boundary crossed.                                                                       | Resolve via upstream feed-rs patch (Option A) or force safe quick-xml constraint (Option B) by 2026-09-30 (I7).                                                          |
| `RUSTSEC-2026-0195` | `quick-xml` general via `feed-rs 2.3`            | Same ingestion path; same feed-rs upstream constraint.                                                                                                               | Same as 2026-0194; related to the same transitive chain.                                                                              | Resolve via upstream feed-rs patch (Option A) or force safe quick-xml constraint (Option B) by 2026-09-30 (I7).                                                          |
| `RUSTSEC-2023-0071` | `rsa` via `jsonwebtoken` `rust_crypto` provider  | Provider selected by owner decision; no upstream patch exists ("No safe upgrade is available"). Vulnerable code is linked but not reachable — see the section below. | None reachable: tokens are `HS256`, no RSA key material exists, and `decode` refuses a foreign `alg` before instantiating a verifier. | Re-review 2026-09-30. `rsa` will not be patched on that horizon; the realistic exits are switching the provider to `aws_lc_rs` or upstream adopting a constant-time RSA. |

## RUSTSEC-2023-0071 — what it actually exposes here

Adopting `rust_crypto` as the `jsonwebtoken` provider (see `Cargo.toml`) pulls `rsa 0.9.10`, which carries the Marvin timing-sidechannel advisory. It is a real dependency of the production build, not a dev-only edge:

```
$ cargo tree -p feedmind-api --edges normal -i rsa
rsa v0.9.10
└── jsonwebtoken v10.4.0
    └── feedmind-api v0.1.0
```

**The vulnerable path is not reachable in this repository.** Three independent reasons, each verified against the resolved source of `jsonwebtoken 10.4.0`:

1. **Nothing here selects an RSA algorithm.** Every signature is minted with `Header::default()` and every verification uses `Validation::default()`; both resolve to `Algorithm::HS256`. `rust_crypto` dispatches `HS256` to its `hmac` backend, and reaches its `rsa` backend only for `RS256/384/512` and `PS256/384/512`, which no call site requests.
2. **A forged `alg` header cannot redirect it.** The token header is attacker-controlled, but `decode` compares it against `validation.algorithms` and returns `InvalidAlgorithm` _before_ it builds a verifier (`decoding.rs:283`, the factory call is on line 287). A token announcing `RS256` is refused without any RSA code executing. This is pinned by the `token_announcing_another_algorithm_is_refused` test.
3. **There is no RSA key material to leak.** Keys are `EncodingKey::from_secret` / `DecodingKey::from_secret`, i.e. an HMAC shared secret. The advisory describes recovery of an RSA _private_ key through timing observation of private-key operations; no such key or operation exists here. The remaining `rsa` entry point in the provider, `extract_rsa_public_key_components`, is only invoked through the JWK helpers, and this repository has no JWK call site.

The honest residual: `rsa` is compiled into the binary and enlarges the audited supply chain, and it adds a second path to the pre-existing `spin 0.9.8` yanked-crate warning (already present via `tracing-subscriber → sharded-slab`). Neither changes the reachability conclusion above.

**Condition attached to this waiver.** The reasoning holds only while the auth boundary stays HS256-only. It collapses if a later change accepts an RSA algorithm in `Validation`, mints RSA tokens, or introduces JWK verification. Reason 2 is mechanically guarded by the algorithm test; the other two are not, so any move to asymmetric JWTs must re-open this ADR rather than inherit the waiver.

**Rejected alternative, recorded for the re-review.** `aws_lc_rs` is the other provider `jsonwebtoken` offers, and `aws-lc-rs 1.17.1` is _already_ compiled into this graph through `rustls` (via `reqwest` and `sqlx-core`). Selecting it would have introduced no new crate and no new advisory. It was not retained: the provider choice is the owner's, taken with the `rsa` consequence named. This paragraph exists so the 2026-09-30 review starts from the actual trade-off rather than re-deriving it.

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
4. Resolve `quick-xml` advisories via upstream patch or safe version constraint (RUSTSEC-2026-0194, 2026-0195); deadline 2026-09-30.
5. Re-review `RUSTSEC-2023-0071` by 2026-09-30. Unlike the others this one has no upstream fix to wait for, so the review decides between keeping `rust_crypto` on the unreachability argument above and switching the provider to `aws_lc_rs`. Re-open this ADR before any move to asymmetric (RSA/JWK) JWTs, which would void the argument.
6. Remove advisory ignores when fixed.

## Acceptance impact

With this ADR and `deny.toml`, advisory risk is explicit and time-bounded. FeedMind may be considered ready for planning-only harness packaging only if all other gates pass and the proof records the waiver reference.
