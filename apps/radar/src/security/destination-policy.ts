// Radar destination policy — the pure, pre-network SSRF gate (docs/apps/radar.md
// §Authentication: "Authorizer validates destination policy again before network
// access"; §Refusal matrix: url_scheme_forbidden / destination_forbidden /
// invalid_source / redirect_forbidden / invalid_limits). Given a candidate URL
// and the addresses it resolves to, this decides whether a worker may connect,
// fail-closed: anything not provably a public unicast destination is refused.
// DNS resolution itself is I/O owned by the worker; this module classifies the
// addresses it is handed, and rejects the fetch if ANY resolved address is
// forbidden (defeating DNS-rebinding).

export type RadarRefusal =
  | "radar.url_scheme_forbidden"
  | "radar.destination_forbidden"
  | "radar.invalid_source"
  | "radar.invalid_limits"
  | "radar.redirect_forbidden";

export type PolicyResult<T> =
  | { readonly ok: true; readonly value: T }
  | { readonly ok: false; readonly refusal: RadarRefusal };

export interface FetchTarget {
  readonly url: string;
  readonly hostname: string;
  readonly port: string;
  readonly isIpLiteral: boolean;
}

export interface FetchLimits {
  readonly maxBytes: number;
  readonly timeoutMs: number;
  readonly maxRedirects: number;
}

function refuse<T>(refusal: RadarRefusal): PolicyResult<T> {
  return { ok: false, refusal };
}

// --- IPv4 -------------------------------------------------------------------

// Parse a strict, canonical dotted-quad to a 32-bit integer, or null. Leading
// zeros are rejected: they are non-canonical and invite octal/decimal ambiguity.
function parseIpv4(ip: string): number | null {
  const parts = ip.split(".");
  if (parts.length !== 4) return null;
  let value = 0;
  for (const part of parts) {
    if (!/^[0-9]{1,3}$/.test(part)) return null;
    if (part.length > 1 && part[0] === "0") return null;
    const octet = Number(part);
    if (octet > 255) return null;
    value = value * 256 + octet;
  }
  return value >>> 0;
}

// IANA special-purpose / non-global-unicast IPv4 blocks, as [base, prefixLen].
const IPV4_FORBIDDEN: readonly (readonly [number, number])[] = [
  [0x00000000, 8], // 0.0.0.0/8    this-network / unspecified
  [0x0a000000, 8], // 10.0.0.0/8   private
  [0x64400000, 10], // 100.64.0.0/10 carrier-grade NAT
  [0x7f000000, 8], // 127.0.0.0/8  loopback
  [0xa9fe0000, 16], // 169.254.0.0/16 link-local (incl. 169.254.169.254 metadata)
  [0xac100000, 12], // 172.16.0.0/12 private
  [0xc0000000, 24], // 192.0.0.0/24 IETF protocol assignments
  [0xc0000200, 24], // 192.0.2.0/24 TEST-NET-1
  [0xc0586300, 24], // 192.88.99.0/24 6to4 relay anycast
  [0xc0a80000, 16], // 192.168.0.0/16 private
  [0xc6120000, 15], // 198.18.0.0/15 benchmarking
  [0xc6336400, 24], // 198.51.100.0/24 TEST-NET-2
  [0xcb007100, 24], // 203.0.113.0/24 TEST-NET-3
  [0xe0000000, 4], // 224.0.0.0/4  multicast
  [0xf0000000, 4], // 240.0.0.0/4  reserved (incl. 255.255.255.255 broadcast)
];

function ipv4Forbidden(ip: number): boolean {
  for (const [base, prefix] of IPV4_FORBIDDEN) {
    const mask = prefix === 0 ? 0 : (0xffffffff << (32 - prefix)) >>> 0;
    if ((ip & mask) >>> 0 === (base & mask) >>> 0) return true;
  }
  return false;
}

// --- IPv6 -------------------------------------------------------------------

// Parse a full or "::"-compressed IPv6 literal (optionally with a trailing
// embedded IPv4) to a 128-bit BigInt, or null. Zone ids are rejected.
function parseIpv6(ip: string): bigint | null {
  if (ip.includes("%")) return null;

  const compressionAt = ip.indexOf("::");
  const compressed = compressionAt !== -1;
  if (compressed && ip.indexOf("::", compressionAt + 2) !== -1) return null;

  const head = compressed ? ip.slice(0, compressionAt) : ip;
  const tail = compressed ? ip.slice(compressionAt + 2) : "";

  const expand = (segment: string): string[] | null => {
    if (segment === "") return [];
    const parts = segment.split(":");
    const hextets: string[] = [];
    for (let index = 0; index < parts.length; index += 1) {
      const part = parts[index];
      if (part === undefined) return null;
      if (part.includes(".")) {
        if (index !== parts.length - 1) return null; // embedded IPv4 only at the end
        const embedded = parseIpv4(part);
        if (embedded === null) return null;
        hextets.push(((embedded >>> 16) & 0xffff).toString(16));
        hextets.push((embedded & 0xffff).toString(16));
      } else {
        if (!/^[0-9a-fA-F]{1,4}$/.test(part)) return null;
        hextets.push(part);
      }
    }
    return hextets;
  };

  const headHextets = expand(head);
  const tailHextets = expand(tail);
  if (headHextets === null || tailHextets === null) return null;

  let hextets: string[];
  if (compressed) {
    const missing = 8 - (headHextets.length + tailHextets.length);
    if (missing < 1) return null; // "::" must stand for at least one zero group
    hextets = [...headHextets, ...Array<string>(missing).fill("0"), ...tailHextets];
  } else {
    hextets = headHextets;
  }
  if (hextets.length !== 8) return null;

  let value = 0n;
  for (const hextet of hextets) value = (value << 16n) + BigInt(Number.parseInt(hextet, 16));
  return value;
}

function ipv6Forbidden(value: bigint): boolean {
  if (value === 0n) return true; // ::            unspecified
  if (value === 1n) return true; // ::1           loopback
  if (value >> 32n === 0xffffn) {
    // ::ffff:0:0/96 IPv4-mapped — classify the embedded IPv4 (public stays allowed).
    return ipv4Forbidden(Number(value & 0xffffffffn));
  }
  if (value >> 121n === 0x7en) return true; // fc00::/7   unique-local
  if (value >> 118n === 0x3fan) return true; // fe80::/10  link-local
  if (value >> 120n === 0xffn) return true; // ff00::/8   multicast
  if (value >> 96n === 0x20010db8n) return true; // 2001:db8::/32 documentation
  return false;
}

// --- Public classification --------------------------------------------------

/** True if `ip` is not provably a public unicast address (fail-closed default). */
export function isForbiddenDestination(ip: string): boolean {
  const v4 = parseIpv4(ip);
  if (v4 !== null) return ipv4Forbidden(v4);
  const v6 = parseIpv6(ip);
  if (v6 !== null) return ipv6Forbidden(v6);
  return true;
}

function stripBrackets(hostname: string): string {
  return hostname.startsWith("[") && hostname.endsWith("]") ? hostname.slice(1, -1) : hostname;
}

function isIpLiteral(host: string): boolean {
  return parseIpv4(host) !== null || parseIpv6(host) !== null;
}

/**
 * Validate the scheme and shape of a candidate feed URL. Refuses a non-HTTP(S)
 * scheme (url_scheme_forbidden), and userinfo or a missing host as an invalid
 * source (invalid_source) — userinfo enables credential smuggling and host
 * confusion.
 */
export function parseFetchTarget(url: string): PolicyResult<FetchTarget> {
  let parsed: URL;
  try {
    parsed = new URL(url);
  } catch {
    return refuse("radar.invalid_source");
  }
  if (parsed.protocol !== "http:" && parsed.protocol !== "https:") {
    return refuse("radar.url_scheme_forbidden");
  }
  if (parsed.username !== "" || parsed.password !== "") return refuse("radar.invalid_source");
  const host = stripBrackets(parsed.hostname);
  if (host === "") return refuse("radar.invalid_source");
  return {
    ok: true,
    value: { url: parsed.href, hostname: host, port: parsed.port, isIpLiteral: isIpLiteral(host) },
  };
}

/**
 * Decide whether a worker may connect to `url`. An IP-literal host is classified
 * directly; a named host is judged against the addresses the caller resolved it
 * to — and the fetch is refused if the resolver returned nothing (fail-closed)
 * or if ANY resolved address is forbidden (defeating DNS-rebinding).
 */
export function evaluateFetchDestination(
  url: string,
  resolvedIps: readonly string[],
): PolicyResult<FetchTarget> {
  const target = parseFetchTarget(url);
  if (!target.ok) return target;

  if (target.value.isIpLiteral) {
    return isForbiddenDestination(target.value.hostname)
      ? refuse("radar.destination_forbidden")
      : target;
  }

  if (resolvedIps.length === 0) return refuse("radar.destination_forbidden");
  for (const ip of resolvedIps) {
    if (isForbiddenDestination(ip)) return refuse("radar.destination_forbidden");
  }
  return target;
}

/** Validate the contract fetch limits (feed-fetch.v1 ranges) — else invalid_limits. */
export function validateLimits(limits: FetchLimits): PolicyResult<FetchLimits> {
  const inRange = (value: number, min: number, max: number): boolean =>
    Number.isInteger(value) && value >= min && value <= max;
  if (!inRange(limits.maxBytes, 1, 10485760)) return refuse("radar.invalid_limits");
  if (!inRange(limits.timeoutMs, 100, 30000)) return refuse("radar.invalid_limits");
  if (!inRange(limits.maxRedirects, 0, 5)) return refuse("radar.invalid_limits");
  return { ok: true, value: limits };
}

/**
 * Guard a redirect hop. A hop past the bound, or one that resolves to a
 * forbidden destination, is refused as `radar.redirect_forbidden` (the matrix
 * folds both cases under that code) — a redirect must never become an SSRF
 * escape hatch.
 */
export function checkRedirect(
  currentRedirects: number,
  maxRedirects: number,
  targetUrl: string,
  resolvedIps: readonly string[],
): PolicyResult<FetchTarget> {
  if (!Number.isInteger(currentRedirects) || currentRedirects < 0) {
    return refuse("radar.redirect_forbidden");
  }
  if (currentRedirects + 1 > maxRedirects) return refuse("radar.redirect_forbidden");
  const destination = evaluateFetchDestination(targetUrl, resolvedIps);
  return destination.ok ? destination : refuse("radar.redirect_forbidden");
}
