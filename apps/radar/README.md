# @libre-ai/radar

Radar lets a person subscribe to chosen feeds, apply visible deterministic rules,
inspect why items were retained or rejected, and export a curated set. Workers
fetch **untrusted** sources; no source becomes trusted content by ingestion
alone.

Work package: `WP-G3-R01`.

## Increment 1 — destination policy (SSRF gate)

`src/security/destination-policy.ts` is the pure, pre-network gate a worker must
clear before opening any connection (spec §Authentication: "Authorizer validates
destination policy again before network access"). It is **fail-closed**: anything
not provably a public unicast destination is refused.

| Function                   | Guarantee                                                                                                                                                                                                      |
| -------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `isForbiddenDestination`   | classifies an IPv4/IPv6 literal against the IANA special-purpose denylist (loopback, private, link-local incl. `169.254.169.254` metadata, CGNAT, multicast, reserved, documentation); unparseable → forbidden |
| `parseFetchTarget`         | `radar.url_scheme_forbidden` for non-HTTP(S); `radar.invalid_source` for userinfo or a missing host                                                                                                            |
| `evaluateFetchDestination` | IP-literal hosts classified directly; named hosts judged against **every** resolved address (defeating DNS-rebinding); no resolved address → `radar.destination_forbidden`                                     |
| `validateLimits`           | enforces the `feed-fetch.v1` ranges (bytes/timeout/redirects) → `radar.invalid_limits`                                                                                                                         |
| `checkRedirect`            | a hop past the bound or to a forbidden destination → `radar.redirect_forbidden`                                                                                                                                |

IPv6 handling covers `::`-compression and IPv4-mapped `::ffff:a.b.c.d` (the
embedded IPv4 is classified, so a mapped loopback/metadata address is refused
while a mapped public one is allowed). DNS resolution itself is I/O owned by the
worker; this module classifies the addresses it is handed.

### Not yet built (deliberately deferred)

- The **hostile feed parser** and **deterministic rule evaluator** — the
  candidate Rust/WASM engine boundary; this step implements no engine.
- The **network quarantine / worker lease** model, tenant persistence with
  PostgreSQL RLS, the API, and the cockpit UI.

## License

EUPL-1.2.
