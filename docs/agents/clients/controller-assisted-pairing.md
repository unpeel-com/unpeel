# Controller-Assisted Pairing

Controller-assisted pairing lets an already-authorized Mac Controller add an
iPhone or iPad to a remote Unpeel Host without opening a screen on that Host.
It is provider-neutral once the assisting Mac already has a supported Direct
or Link connection: the Host may be another Mac, a Linux server, a VPS, or a
managed container. The provider is only where the Host runs; it does not
participate in Unpeel's pairing protocol. This flow does not bootstrap the
first Controller connection. Upstash Box currently accepts only interactive
SSH, so it is validated as an in-shell Host/browser runtime but not yet as a
Mac Host-picker or phone target.

Status: implemented at Host protocol minor 7 through capability
`pairing.invitation` for native and headless Hosts over Direct and Unpeel Link.
The assisting Mac's proxy is a dedicated Controller-side one-shot listener in
both client-only Dev and released compatibility paths; it is not part of a
Swift or Rust Host server.

## Roles and requirements

- **Host** — owns Sessions and credentials. It runs the canonical workspace
  worker (or a released compatibility Host) and advertises
  `pairing.invitation`.
- **Assisting Controller** — a Mac app already paired with and currently
  connected to the Host. It displays the QR and exposes a short-lived local
  pairing proxy.
- **Joining Controller** — the iPhone or iPad being added. It must run a client
  that understands the optional proxy field in the compact pairing code.

The joining phone must reach the assisting Mac once over LAN or VPN. The Mac
must be able to reach the Host over its already-accepted Direct or Link
connection. The phone does not need direct network access to the Host during
pairing.

## User flow

1. In the Mac app, select the remote Host.
2. Open **Settings → Remote** and choose
   **Add iPhone or iPad to Host Name…**.
3. On the phone, open **Your Devices → Add a Device** and scan the QR.
4. Keep the sheet open until **Controller paired** appears.
5. After pairing, the phone connects to the Host itself: Direct when the saved
   Host endpoint is reachable, otherwise through Unpeel Link when enabled.

Closing the Mac sheet invalidates its local proxy immediately. Refreshing the
QR replaces the previous invitation, and each invitation expires after five
minutes.

## General protocol

```text
Remote Host              Assisting Mac                 Joining phone
    |                           |                             |
    |<-- authenticated create --|                             |
    |--- one-time QR payload -->|--- QR shown/scanned ------->|
    |                           |                             |
    |                           |<-- sealed request ----------|
    |<-- authenticated complete-|                             |
    |--- sealed credentials --->|--- unchanged response ----->|
    |                           |                             |
    |<============= phone connects Direct or via Link =======>|
```

The concrete sequence is:

1. The Mac starts/reserves a random
   `/mobile/pairing-proxy/<proxy-id>` base URL on its dedicated short-lived
   Controller listener. That listener exposes no Host routes; its only
   exchange route is `POST <base>/pair`.
2. The Mac sends an authenticated, generation-bound request to the selected
   Host:

   ```json
   {
     "action": "create",
     "endpoint": "http://controller:PORT/mobile/pairing-proxy/PROXY-ID"
   }
   ```

3. The Host opens its normal single-use pairing window, binds it to that proxy
   endpoint, and returns a `RemotePairingPayload`. The compact QR adds the
   proxy id as an optional eighth field.
4. The phone derives the pairing key from the QR token and seals its device
   request. The authenticated data binds the direction, Host id, and exact
   proxy endpoint.
5. The phone posts that sealed envelope to the Mac proxy. The proxy is
   intentionally unauthenticated HTTP because possession of the short-lived QR
   token is the pairing authority; the random proxy id prevents unrelated
   traffic from reaching the forwarding path.
6. The Mac validates the envelope's wire shape and sends it to the Host as a
   `complete` pairing-invitation effect. It does not decrypt the envelope.
7. The Host decrypts and consumes the invitation, creates credentials unique
   to the phone, and seals the response to the same QR context. The response
   includes:

   - the proxy URL in `endpoint`, preserving the cryptographic binding;
   - the Host's real `/mobile` URL in optional `directEndpoint`;
   - the phone bearer, E2E material, and Link credential.

8. The Mac forwards the sealed response unchanged and removes the proxy. The
   phone validates the response and persists `directEndpoint` instead of the
   one-time proxy URL.
9. Normal Controller routing begins. Direct is preferred; Link is the fallback
   for reachability failure.

## Security and trust boundary

- Only the Host mints or persists the joining phone's durable credentials.
- The assisting Mac sees the displayed one-time QR secret, but the phone's
  request and the Host's credential response stay encrypted end to end.
- Pairing-invitation calls use the same authenticated Host connection,
  advertised capability checks, connection generation, and no-automatic-replay
  effect rules as other remote mutations.
- An uncertain `complete` result is never automatically retried: the Host may
  already have created the phone credential.
- The Host supports one active pairing window. Creating a new local or assisted
  invitation invalidates the previous one.
- The proxy transports only the sealed pairing exchange. It is not a general
  Controller-to-Host tunnel and never carries Session content.
- Unpeel Link remains an opaque transport and does not store pairing or Session
  content.

Current Direct desktop transport is bearer-authenticated plaintext HTTP and is
for trusted LAN/VPN use. Controller-assisted pairing does not change that
boundary or expose the remote Host's HTTP port publicly.

## General deployment checklist

This is the same for any remote-host provider:

1. Run a current native or headless Unpeel Host with durable `~/.unpeel`
   storage. Container filesystems must persist this directory across restarts.
2. Pair the Mac to that Host once and verify the Host picker reports Direct or
   Via Link.
3. Enable Link for that Host if the phone will not normally share its private
   network.
4. Put the phone and assisting Mac on the same LAN or VPN for the one-time QR
   exchange.
5. Use **Add iPhone or iPad to Host Name…**, then verify the phone lists the
   remote Host and can load its Sessions after leaving the pairing screen.

No provider-specific pairing service, public inbound Host port, or browser is
required. Browser support on the remote box is a separate Host capability used
by agents after the Controller connection exists.

## Troubleshooting

- **Add iPhone or iPad to Host Name is disabled** — the selected Host did not
  advertise `pairing.invitation`; update Unpeel on the Host and reconnect.
- **Creating invitation never produces a QR** — verify the Mac is still
  connected to the selected Host and that its short-lived Controller proxy
  can bind a local port.
- **The phone cannot submit the scanned code** — the phone cannot reach the
  assisting Mac. Join the same LAN/VPN and check local firewall isolation.
- **The code expired** — refresh the QR. A refreshed code invalidates the old
  one.
- **Pairing succeeds but the Host is offline afterward** — the saved Direct
  endpoint is unreachable and Link is unavailable or disabled. Enable Link or
  connect the phone to the Host's LAN/VPN.
- **An older phone rejects the QR** — update the iOS app; older decoders accept
  only the seven-field direct pairing code.
- **The response was lost after the Host created a device** — inspect the
  Host's paired-device list, revoke the orphaned entry if necessary, and create
  a fresh invitation. Do not replay the old `complete` request.

## What this is not

This is not an account-backed guest invitation. A person whose phone cannot
reach the assisting Mac still needs the future operated Link invitation and
membership flow. That flow may distribute authority through the Link control
plane, but it must not persist Session or room content there.

It is also not live Session handoff. Pairing gives another Controller access to
the same Host; moving work to a different Host remains restart-with-resume.

## Implementation map

- Capability and conformance:
  `protocol/host-capabilities-v1.json`,
  `protocol/host-conformance-v1.json`
- Shared QR, pairing response, and validation:
  `clients/shared/UnpeelShared/Sources/UnpeelShared/RemoteControlProtocol.swift`,
  `RemotePairingClient.swift`
- Mac invitation UI and proxy lifecycle:
  `clients/native/UnpeelNative/Sources/UnpeelNative/Views/HostPickerView.swift`,
  `UnpeelStore.swift`, `ControllerPairingProxy.swift`
- Generation-bound Controller backend:
  `crates/unpeel-core/src/remote_session_backend.rs`,
  `crates/unpeel-native-bridge/src/lib.rs`
- Headless Direct and Link Host adapters:
  `crates/unpeel-cli/src/mobile.rs`, `pairing.rs`, `relay.rs`
- Phone persistence of the real Host endpoint:
  `clients/ios/UnpeelIOS/Sources/UnpeelIOS/RemoteConnectionStore.swift`

## Rules for extensions

- Keep one Host operation and one wire shape for native and headless Hosts.
- Advertise the capability; never infer support from Host kind or a 404 probe.
- Keep the QR endpoint as the pairing AAD and add alternate steady-state
  endpoints as authenticated response fields.
- Never expose the joining Controller's plaintext credential to the assisting
  Controller or Relay.
- Treat invitation creation and completion as at-most-once effects and bind
  them to the accepted Host connection generation.
- Add positive and negative cases to the shared conformance fixture whenever
  route behavior changes.
