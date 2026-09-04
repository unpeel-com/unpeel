# Security Policy

Unpeel's core promise is that your sessions run on hardware you own and
nothing leaves your machines. Security reports that test that promise are
exactly what we want to hear about.

## Reporting a vulnerability

Email **support@unpeel.com** with `SECURITY` in the subject line. Please
include reproduction steps and affected component/version. You'll get a
human reply; give us a reasonable window to fix before public disclosure.
Please do **not** open a public issue for vulnerabilities.

## Scope

In scope, in rough order of how much we care:

- The E2E relay protocol and its implementations (the `unpeel-relay` repository,
  `RelayProtocol.swift`, the Rust uplink) — anything that lets the relay or a
  network observer read or forge session content breaks the core promise.
- Host exposure: the remote-control server (TLS/pairing/auth), the hook HTTP
  server, the MCP trust boundary (cross-group writes, approval bypasses),
  and the SSH gateway.
- The clients (macOS, iOS, TUI): sandbox escapes via terminal output,
  credential handling, pairing spoofing.
- License/entitlement forgery.
- The operated services (unpeel.com accounts/licensing, the relay): please
  test only against your own account and data.

Out of scope: what CLI agents *themselves* do inside a session you launched
(that's the agent vendor's domain), denial of service against your own
self-hosted infrastructure, and social engineering.

## No bounty (yet)

There is currently no paid bounty program — reports get fast fixes, credit
in release notes if you want it, and our genuine gratitude.
