# Viewer presence and terminal sizing

Presence currently identifies **devices viewing a Session**, including phones,
iPads, and Macs. It does not establish human membership, permissions, or an
exclusive input/resize owner.

The Host publishes two existing observation feeds under its `remote/` directory:

- `presence.json`: terminal stream/poll viewers; the native reader expires
  entries after 20 seconds.
- `mobile-presence.json`: authenticated Direct/Link output leases, expiring
  after 15 seconds. The filename is legacy; paired Macs use this feed too.

`ViewerPresenceStore` merges these feeds by authenticated device id, extracted
from the shipped `Name (id)` label. It expires each feed before merging and
keeps the newest live record. Names, network addresses, and transport choice
are not authenticated identity. Unidentified legacy viewers remain distinct
from paired devices. One device crossing transports or viewing several
Sessions produces one connection notification, not a new arrival per stream.
Notification suppression still targets the exact viewing device.

`TerminalPresenceView` renders device chips and the shared-grid recovery action
in each pane header, including plain shells. Chips use device labels and stable
colors; the client does not infer an iPhone from an endpoint name or a human's
account picture from a matching device name.

Mobile fit-to-screen changes the **same hosted PTY grid** seen on desktop.
The existing `phone-fit.json` / `phoneResizeOverrides` projection remains the
fit authority. Its marker persists independently of viewer liveness and has
no device-owner field. The header therefore presents “Fit to desktop” beside
presence without assigning the fit to a particular viewer or inventing a
connected phone when only the marker remains. Merely watching does not claim
exclusive terminal control.

Automatic local desktop grid restoration waits until every viewer has left
and no explicit fit remains. A Session that had viewers retains a one-shot
forced-refit candidate until a mounted pane can use it. The deferred AppKit
operation rechecks selection, Host scope, fit, and presence before resizing.
Explicit “Fit to desktop” remains available while another device is viewing.

The disk feed belongs only to this process's own Host. Other workspace/remote
scopes must never use it merely because a Session id matches. Those scopes
currently have no viewer-list projection in the Host protocol, so their pane
headers do not invent a list. Their existing fit projection remains unchanged.

For full shared workspaces, add authenticated viewer identity and presence to
the Host protocol first (capability and conformance fixtures), then consume it
in the same client model and header. Human membership, roles, control handoff,
and per-user unread state are separate Host features; these device leases do
not grant any of them.
