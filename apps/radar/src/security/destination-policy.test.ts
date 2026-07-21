import { describe, expect, test } from "bun:test";

import {
  checkRedirect,
  evaluateFetchDestination,
  isForbiddenDestination,
  parseFetchTarget,
  validateLimits,
} from "./destination-policy";

describe("isForbiddenDestination — IPv4", () => {
  test.each([
    "127.0.0.1", // loopback
    "10.0.0.1", // private
    "172.16.0.0", // private lower bound
    "172.31.255.255", // private upper bound
    "192.168.1.1", // private
    "169.254.169.254", // cloud metadata
    "169.254.0.1", // link-local
    "100.64.0.1", // CGNAT
    "100.127.255.255", // CGNAT upper bound
    "0.0.0.0", // this-network
    "224.0.0.1", // multicast
    "255.255.255.255", // broadcast (240/4)
    "192.0.2.1", // TEST-NET-1
    "198.51.100.7", // TEST-NET-2
    "203.0.113.9", // TEST-NET-3
    "198.18.0.1", // benchmarking
  ])("forbids %s", (ip) => {
    expect(isForbiddenDestination(ip)).toBe(true);
  });

  test.each([
    "8.8.8.8",
    "1.1.1.1",
    "93.184.216.34",
    "11.0.0.1",
    "172.15.255.255", // just below 172.16/12
    "172.32.0.1", // just above 172.16/12
    "100.63.255.255", // just below CGNAT
    "100.128.0.0", // just above CGNAT
    "169.253.255.255", // just below link-local
  ])("allows public %s", (ip) => {
    expect(isForbiddenDestination(ip)).toBe(false);
  });
});

describe("isForbiddenDestination — IPv6", () => {
  test.each([
    "::1", // loopback
    "::", // unspecified
    "fe80::1", // link-local
    "fc00::1", // unique-local
    "fd00::1", // unique-local
    "ff02::1", // multicast
    "2001:db8::1", // documentation
    "::ffff:127.0.0.1", // IPv4-mapped loopback
    "::ffff:169.254.169.254", // IPv4-mapped metadata
    "::ffff:192.168.0.1", // IPv4-mapped private
  ])("forbids %s", (ip) => {
    expect(isForbiddenDestination(ip)).toBe(true);
  });

  test.each([
    "2606:4700:4700::1111", // Cloudflare public
    "2001:4860:4860::8888", // Google public
    "::ffff:8.8.8.8", // IPv4-mapped public stays allowed
  ])("allows public %s", (ip) => {
    expect(isForbiddenDestination(ip)).toBe(false);
  });
});

describe("isForbiddenDestination — fail-closed on garbage", () => {
  test.each([
    "",
    "not-an-ip",
    "999.999.999.999",
    "1.2.3",
    "1.2.3.4.5",
    "127.0.0.01",
    "::gg",
    "1::2::3",
  ])("forbids unparseable %p", (value) => {
    expect(isForbiddenDestination(value)).toBe(true);
  });
});

describe("parseFetchTarget", () => {
  test("accepts a plain https URL", () => {
    const result = parseFetchTarget("https://example.com/feed.xml");
    expect(result.ok).toBe(true);
    if (!result.ok) return;
    expect(result.value.hostname).toBe("example.com");
    expect(result.value.isIpLiteral).toBe(false);
  });

  test.each([
    "ftp://example.com/x",
    "file:///etc/passwd",
    "data:text/plain,hi",
    "gopher://example.com",
  ])("refuses a non-HTTP(S) scheme: %s", (url) => {
    expect(parseFetchTarget(url)).toEqual({ ok: false, refusal: "radar.url_scheme_forbidden" });
  });

  test.each([
    "https://user:pass@example.com/", // userinfo
    "https://user@example.com/", // username only
    "not a url",
    "https://", // no host at all
  ])("refuses an invalid source: %s", (url) => {
    expect(parseFetchTarget(url)).toEqual({ ok: false, refusal: "radar.invalid_source" });
  });
});

describe("evaluateFetchDestination", () => {
  test("allows a public IP literal", () => {
    expect(evaluateFetchDestination("https://8.8.8.8/feed", []).ok).toBe(true);
  });

  test.each([
    ["https://127.0.0.1/feed", []],
    ["http://[::1]/feed", []],
    ["http://169.254.169.254/latest/meta-data", []],
  ])("forbids a private/loopback/metadata IP literal: %s", (url, ips) => {
    expect(evaluateFetchDestination(url, ips)).toEqual({
      ok: false,
      refusal: "radar.destination_forbidden",
    });
  });

  test("allows a named host that resolves only to public addresses", () => {
    expect(evaluateFetchDestination("https://example.com/feed", ["93.184.216.34"]).ok).toBe(true);
  });

  test("forbids a named host with no resolved addresses (fail-closed)", () => {
    expect(evaluateFetchDestination("https://example.com/feed", [])).toEqual({
      ok: false,
      refusal: "radar.destination_forbidden",
    });
  });

  test("forbids a named host if ANY resolved address is private (anti-rebinding)", () => {
    expect(
      evaluateFetchDestination("https://rebind.example/feed", ["93.184.216.34", "127.0.0.1"]),
    ).toEqual({ ok: false, refusal: "radar.destination_forbidden" });
  });

  test("propagates a scheme refusal", () => {
    expect(evaluateFetchDestination("ftp://example.com/feed", [])).toEqual({
      ok: false,
      refusal: "radar.url_scheme_forbidden",
    });
  });
});

describe("validateLimits", () => {
  test("accepts limits inside the contract ranges", () => {
    expect(validateLimits({ maxBytes: 1048576, timeoutMs: 5000, maxRedirects: 3 }).ok).toBe(true);
  });

  test.each([
    { maxBytes: 0, timeoutMs: 5000, maxRedirects: 3 },
    { maxBytes: 10485761, timeoutMs: 5000, maxRedirects: 3 },
    { maxBytes: 1048576, timeoutMs: 99, maxRedirects: 3 },
    { maxBytes: 1048576, timeoutMs: 30001, maxRedirects: 3 },
    { maxBytes: 1048576, timeoutMs: 5000, maxRedirects: -1 },
    { maxBytes: 1048576, timeoutMs: 5000, maxRedirects: 6 },
    { maxBytes: 1.5, timeoutMs: 5000, maxRedirects: 3 },
  ])("refuses out-of-range limits: %o", (limits) => {
    expect(validateLimits(limits)).toEqual({ ok: false, refusal: "radar.invalid_limits" });
  });

  test.each([
    { maxBytes: 10485760, timeoutMs: 30000, maxRedirects: 5 },
    { maxBytes: 1, timeoutMs: 100, maxRedirects: 0 },
  ])("accepts the exact boundaries: %o", (limits) => {
    expect(validateLimits(limits).ok).toBe(true);
  });
});

describe("checkRedirect", () => {
  test("allows a hop within the bound to a public destination", () => {
    expect(checkRedirect(0, 5, "https://8.8.8.8/next", []).ok).toBe(true);
  });

  test("refuses a hop past the bound", () => {
    expect(checkRedirect(5, 5, "https://8.8.8.8/next", [])).toEqual({
      ok: false,
      refusal: "radar.redirect_forbidden",
    });
  });

  test("refuses a zero-redirect budget", () => {
    expect(checkRedirect(0, 0, "https://8.8.8.8/next", [])).toEqual({
      ok: false,
      refusal: "radar.redirect_forbidden",
    });
  });

  test("refuses a redirect to a forbidden destination as redirect_forbidden", () => {
    expect(checkRedirect(1, 5, "https://127.0.0.1/next", [])).toEqual({
      ok: false,
      refusal: "radar.redirect_forbidden",
    });
  });

  test("refuses a redirect that changes scheme as redirect_forbidden", () => {
    expect(checkRedirect(1, 5, "ftp://example.com/next", ["93.184.216.34"])).toEqual({
      ok: false,
      refusal: "radar.redirect_forbidden",
    });
  });
});
