# UnpeelShared

Swift package with the client-side protocol and crypto shared by the Mac app
(`apps/native`) and the iPhone/iPad app (`apps/ios`) — one implementation so
the two clients can never drift on wire behavior:

- `RemoteControlProtocol.swift` — the Host remote-control contract (session
  listing, output streaming, verbs) spoken to both Mac-app and headless-TUI
  Hosts
- `RemotePairingClient.swift` — pairing flow; validates the credentials a
  Host hands out (auth token, relay URL + token, 32-byte E2E key)
- `RelayProtocol.swift` / `RemoteRelayConnection.swift` — the forward-secret
  E2E handshake and framing used over the Link relay (server counterpart:
  `apps/relay`)
- `PairedHostRecord.swift` — persisted Host identity (what fail-closed
  reconnects pin against)
- `ToolIcons.swift` / `ChromeIcons.swift` — shared provider/browser icon art

Tests include the relay known-answer vectors
(`Tests/UnpeelSharedTests/RelayCryptoVectorTests.swift`), which consume
`protocol/relay-kat-vectors-v1.json` — the same fixtures the relay's own
JS tests run, keeping both ends of the E2E protocol pinned to identical
bytes.

Run tests: `swift test --package-path apps/shared/UnpeelShared` (this one
works with plain `swift test`, unlike the iOS package).

Everything here is open source and must stay free of closed-service
dependencies: crypto and wire behavior are
exactly the things users need to be able to audit.
