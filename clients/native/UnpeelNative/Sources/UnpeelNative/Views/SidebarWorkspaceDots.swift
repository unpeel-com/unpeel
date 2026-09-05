//
//  SidebarWorkspaceDots.swift
//  UnpeelNative
//
//  Dia-browser-style workspace switching for the sidebar:
//
//  - `WorkspaceSwitching` is THE shared switch semantics — the ordered row
//    list (same rows and saved user order as Settings ▸ Workspaces) plus
//    scoped-check and select. The sidebar picker, settings list, footer dots,
//    and trackpad swipe all
//    route through it; never duplicate the kind-switch again.
//  - `SidebarWorkspaceDots` renders one small dot per workspace in the
//    footer strip — muted marks with a gooey tint blob riding the scoped
//    one (slightly larger, no ring); click switches. During the interactive
//    swipe the blob is the pager indicator: it stretches toward the peeked
//    neighbor with the fingers, collapses onto the new dot on commit, and
//    springs back on cancel.
//  - `SidebarWorkspaceSwipeMonitor` turns a horizontal two-finger trackpad
//    swipe anywhere over the sidebar into an interactive pager built on the
//    standard carousel model: ONE container (`SidebarWorkspaceCarousel`)
//    owns the sidebar list area — a fixed-frame, clipped region exactly the
//    list area's size (its geometry contract is documented on the modifier:
//    page content can never resize the region or move the footer chrome
//    below it) — and translates horizontally, holding TWO
//    materialized pages of the SAME list component (`SidebarListPage`) —
//    the current page driven by the live store exactly as always, and the
//    neighbor page driven read-only from its pooled bootstrap snapshot.
//    The gesture translates the container 1:1 (rubber-banded; the footer
//    and its dots stay put) while the chrome tint scrubs gradually toward
//    the neighbor's color. Passing the commit threshold (or a decisive
//    flick) commits; otherwise the container springs home.
//    `SidebarWorkspacePager` is the tiny published gesture model; the
//    carousel and the footer dots are its only observers. Reduce Motion
//    and phase-less legacy wheel events keep the original instantaneous
//    threshold switch.
//
//  Zero-seam commit, by construction (no peek panel, no freeze, no cover
//  timer, no opacity swap — the page you drag in is the page you land on):
//
//  - Commit animates the container one page over, so the pooled neighbor
//    page lands at x=0 already showing its final pixels. Then ONE
//    unanimated transaction performs the scope switch and re-bases the
//    container to zero: the (now-live) current page renders the same rows
//    the pooled page showed, because the switch seeds the runtime with the
//    exact pooled snapshot that page rendered from (`seedSnapshot` inside
//    selectHost/selectLocalWorkspace), the expansion set is read from the
//    same per-host persisted key on both pages, and both pages render from
//    the TOP — the pooled page has no scroll, and the live page top-lands
//    on scope change via SidebarView's SidebarScrollCue, unanimated,
//    inside that same commit. Identical inputs on both pages by
//    construction — never by synchronization — is what makes the swap
//    invisible. (Per-workspace remembered scroll offsets were considered
//    and rejected: restoring a pixel offset needs the macOS 15
//    ScrollPosition API — the package targets macOS 13 — and id-based
//    scrollTo restoration is approximate under lazy layout.)
//  - Every gesture `begin` fully re-bases from a zero container offset: a
//    commit whose slide is still animating is settled synchronously first
//    (the scope switch lands, unanimated), so a new swipe can never start
//    against a stale offset or a half-committed scope.
//
//  Local switching follows the Workspaces feature; paired/SSH rows are still
//  added only behind the remote Host development gate. With only the default
//  workspace there is nothing to switch, so the footer renders no dots.
//

import AppKit
import SwiftUI
import UnpeelShared

// MARK: - Shared switch semantics

/// One place for "what does clicking workspace row X do": current instance
/// returns to local scope, another local workspace rescopes this window in
/// place over the loopback gateway (selectLocalWorkspace), and paired/SSH
/// Hosts route through the existing selectHost scope path. Used by the
/// sidebar picker, Settings ▸ Workspaces, the footer dots, and the swipe.
@MainActor
enum WorkspaceSwitching {
    /// The unified, user-ordered workspace list — same builder and saved
    /// order as Settings ▸ Workspaces / the sidebar picker.
    static func orderedRows(store: UnpeelStore) -> [WorkspaceListRowModel] {
        WorkspaceListOrder.apply(
            to: WorkspacesSettingsPanel.buildRows(
                store: store,
                includeExtraLocal: true
            ),
            key: \.id
        )
    }

    /// Whether `row` is the workspace the window is currently scoped to.
    static func isScoped(_ row: WorkspaceListRowModel, store: UnpeelStore) -> Bool {
        switch row.kind {
        case let .local(_, _, isCurrentInstance, _):
            if isCurrentInstance {
                return store.selectedHostScope == .local
            }
            return store.selectedHostScope.localWorkspaceHome == row.home
        case .paired(let host):
            return store.selectedHostScope.remoteHostID == host.hostID
        case .ssh(let host):
            return store.selectedHostScope.remoteHostID == host.id
        }
    }

    /// Switch the window's scope to `row`; no-op when already there.
    static func select(_ row: WorkspaceListRowModel, store: UnpeelStore) {
        switch row.kind {
        case let .local(_, _, isCurrentInstance, _):
            if isCurrentInstance {
                // Back to this instance's local scope; no-op when already
                // local.
                if store.selectedHostScope != .local {
                    store.selectHost(nil)
                }
                return
            }
            // Another LOCAL workspace rescopes this window in place over the
            // loopback gateway — no second app instance required, and a
            // simultaneously running instance is fine (hosted sessions are
            // host-based; gateway and app read the same home).
            guard let home = row.home else { return }
            store.selectLocalWorkspace(home: home, name: row.name)
        case .paired(let host):
            store.selectHost(host.hostID)
        case .ssh(let host):
            store.selectHost(host.id)
        }
    }

    /// Click/selector switch (phase 7): ONE direction-aware slide of the
    /// carousel — with the target's REAL pooled page — never paging past
    /// intermediates; a wrap takes the shorter direction, and the gooey
    /// blob morphs straight to the clicked dot. Falls back to the instant
    /// switch whenever the carousel is unavailable: Reduce Motion, the
    /// settings nav covering the sidebar list, no registered pager, or the
    /// row already scoped.
    static func selectSliding(_ row: WorkspaceListRowModel, store: UnpeelStore) {
        guard let animator = store.workspacePagerAnimator,
              !store.settingsVisible,
              !SidebarMotion.reduceMotion
        else {
            select(row, store: store)
            return
        }
        // A commit whose slide is still animating lands its scope switch
        // NOW, so the scoped index below is computed against settled truth.
        animator.settle()
        guard !isScoped(row, store: store) else { return }
        let rows = orderedRows(store: store)
        guard rows.count > 1,
              let currentIndex = rows.firstIndex(where: { isScoped($0, store: store) }),
              let targetIndex = rows.firstIndex(where: { $0.id == row.id }),
              currentIndex != targetIndex
        else {
            select(row, store: store)
            return
        }
        // The target's page enters from the side it sits on in the shared
        // order; equidistant wrap ties go forward.
        let plan = SidebarPagerMath.slidePlan(
            currentIndex: currentIndex,
            targetIndex: targetIndex,
            count: rows.count
        )
        let started = animator.programmaticSwitch(
            to: rows[targetIndex],
            direction: plan.direction,
            isWrap: plan.isWrap
        ) { selected in
            select(selected, store: store)
        }
        if !started {
            select(row, store: store)
        }
    }

    /// Step to the next (+1) / previous (-1) workspace in the shared user
    /// order, wrapping at both ends. Rows are built fresh here — this runs
    /// at most once per completed swipe gesture (the monitor's one-shot
    /// latch), never per scroll event.
    static func cycle(by delta: Int, store: UnpeelStore) {
        let rows = orderedRows(store: store)
        guard rows.count > 1 else { return }
        let current = rows.firstIndex { isScoped($0, store: store) } ?? 0
        let count = rows.count
        let next = ((current + delta) % count + count) % count
        guard next != current else { return }
        select(rows[next], store: store)
    }
}

// MARK: - Footer dots

/// One dot per workspace, in the shared user order: muted gray marks, with
/// a gooey tint blob riding the scoped one (slightly larger, no ring).
/// During the interactive swipe the blob is the pager indicator
/// (`SidebarWorkspacePager.dotsPager` drives it): it stretches from
/// the scoped slot toward the peeked neighbor as the fingers travel,
/// collapses onto the new dot on commit, and springs back on cancel.
/// Rendered as a centered layer over the sidebar footer's 22pt strip; click
/// switches. Rows are cached (registry + liveness + tints are disk-backed)
/// and refreshed on appearance, on the once-per-change tint notification,
/// and on hover — never in a body pass.
struct SidebarWorkspaceDots: View {
    @ObservedObject var store: UnpeelStore
    /// The same instance the swipe monitor drives; observing it here is what
    /// lets the blob track the fingers. This view is tiny — a handful of
    /// circles — so per-scroll-event re-renders stay trivial.
    @ObservedObject var pager: SidebarWorkspacePager
    /// Background workspace pool: attention badges (a background workspace
    /// with a session needing input gets a small orange dot on its slot).
    @ObservedObject var pool: WorkspacePool

    @State private var rows: [WorkspaceListRowModel] = []
    /// Which dot the cursor is over, for the blob's hover growth (the gray
    /// base dots scale themselves; the scoped slot's visual is the blob).
    @State private var hoveredID: String?

    var body: some View {
        if WorkspaceFeature.pickerEnabled {
            content
                // Creating/pairing a workspace immediately selects it. The
                // row cache must refresh even when the one-workspace probe
                // is invisible and therefore cannot receive a hover.
                .onChange(of: store.selectedHostScope) { _ in refresh() }
                .onReceive(
                    NotificationCenter.default.publisher(for: .unpeelWorkspaceTintChanged)
                ) { _ in refresh() }
                .onReceive(
                    NotificationCenter.default.publisher(for: .unpeelWorkspaceListChanged)
                ) { _ in refresh() }
        }
    }

    @ViewBuilder
    private var content: some View {
        if rows.count > 1 {
            let activeIndex = rows.firstIndex {
                WorkspaceSwitching.isScoped($0, store: store)
            } ?? 0
            HStack(spacing: 2) {
                ForEach(Array(rows.enumerated()), id: \.element.id) { index, row in
                    WorkspaceDot(
                        name: row.name,
                        tooltip: tooltip(
                            for: row,
                            selected: index == activeIndex,
                            attention: index != activeIndex
                                && pool.attentionKeys.contains(row.id)
                        ),
                        // The scoped slot's visual is the blob; only
                        // BACKGROUND workspaces badge attention here (the
                        // foreground one shows its own per-session state).
                        attention: index != activeIndex
                            && pool.attentionKeys.contains(row.id)
                    ) {
                        WorkspaceSwitching.selectSliding(row, store: store)
                    } hoverChanged: { inside in
                        if inside {
                            hoveredID = row.id
                        } else if hoveredID == row.id {
                            hoveredID = nil
                        }
                    }
                }
            }
            .overlay(alignment: .leading) {
                gooeyIndicator(activeIndex: activeIndex)
            }
            .frame(height: 22)
            // Hover precedes any click — the lazy moment to re-check
            // tints/order without polling from the footer.
            .background(HoverReporter { inside in
                if inside { refresh() }
            })
            // Primary click keeps the direct dot switch. Secondary click
            // anywhere on this compact strip opens the complete picker.
            .background {
                SidebarWorkspacePopover(
                    store: store,
                    hosts: store.remoteHostStore,
                    pool: pool
                )
            }
            .onAppear(perform: refresh)
        } else {
            // Zero-size probe: visibility depends on the cached rows, which
            // someone must load once per footer appearance. Keeping this
            // mounted for zero/one row lets change notifications reveal the
            // switcher as soon as a second workspace is added.
            Color.clear
                .frame(width: 0, height: 0)
                .onAppear(perform: refresh)
        }
    }

    /// The scoped workspace's tint blob, laid over the muted base dots. At
    /// rest it is a 7pt circle on the active slot; while the swipe pager is
    /// live it becomes the gooey worm between the active slot and the
    /// peeked neighbor's — except when the step WRAPS around an end of the
    /// row (`DotsPager.isWrap`): stretching across every dot in between
    /// reads wrong, so the blob stays a circle on the current dot and, on
    /// commit, simply slides to the wrapped target via the same 0.12s
    /// click-slide (`activeIndex` animation), already wearing the new
    /// workspace's tint. Its fill scrubs from the scoped workspace's swatch
    /// toward the peeked one's with finger progress (`scrubbedTint`, the
    /// chrome wash's trailing curve) and completes during a commit's
    /// collapse. Never hit-tests — the base dot buttons underneath keep
    /// click/tooltip behavior.
    ///
    /// Geometry is resolved HERE into plain endpoint coordinates
    /// (head/tail/radius) and only those are animatable. The shape must
    /// never animate a (fromX, stretch) pair: when a commit's post-select
    /// cleanup re-bases the blob onto the new active slot, interpolating
    /// `fromX` old→target against `stretch` 1→0 bends the head through
    /// `target − Δ·t(1−t)` — a backward dip that read as the blob bouncing
    /// off the target. With endpoints, that cleanup frame is a true no-op
    /// (head = tail = target slot both before and after), so a committed
    /// swipe is one continuous forward settle: the head holds at the target
    /// while the tail flows forward onto it, strictly in the direction of
    /// travel.
    private func gooeyIndicator(activeIndex: Int) -> some View {
        let pager = self.pager.dotsPager
        let active = rows[activeIndex]
        let targetIndex = pager.flatMap { pager in
            rows.firstIndex { $0.id == pager.targetID }
        }
        // Hover growth parity with the plain dots (blob covers its base dot).
        let atRest = targetIndex == nil
        let restRadius: CGFloat = atRest && hoveredID == active.id
            ? 3.5 * 1.25
            : 3.5
        // Wrap = no goo: geometry pinned to the current dot for the whole
        // drag (color still scrubs; the post-select slide moves the blob).
        let noStretch = atRest || (pager?.isWrap ?? false)
        let fromX = Self.slotCenterX(activeIndex)
        let toX = Self.slotCenterX(noStretch ? activeIndex : (targetIndex ?? activeIndex))
        let stretch = noStretch ? 0 : min(1, max(0, pager?.stretch ?? 0))
        let release = noStretch ? 0 : min(1, max(0, pager?.release ?? 0))
        // `stretch` walks the head toward the target, `release` walks the
        // tail across after a commit; both resolve to straight-line endpoint
        // positions before any animation sees them.
        let head = fromX + (toX - fromX) * stretch
        let tail = fromX + (toX - fromX) * release
        // Thin the blob as it elongates: full-height circle at rest and on
        // arrival, ~78% height at maximum stretch.
        let span = abs(toX - fromX)
        let elongation = span > 0 ? abs(head - tail) / span : 0
        let radius = restRadius * (1 - 0.22 * elongation)
        return GooeyDotIndicator(headX: head, tailX: tail, radius: radius)
            .fill(Self.scrubbedTint(
                active: active.tint.swatch,
                target: targetIndex.map { rows[$0].tint.swatch },
                pager: pager
            ))
            .allowsHitTesting(false)
            // Click switches (no pager) and wrap commits slide the blob to
            // the new slot. A goo commit's post-select cleanup changes no
            // endpoint (head and tail already sit on the target slot), so
            // even though activeIndex changes here, nothing moves — the
            // collapse's easeOut is the only motion a commit shows.
            .animation(
                SidebarMotion.reduceMotion ? nil : .easeInOut(duration: 0.12),
                value: activeIndex
            )
            .animation(.easeInOut(duration: 0.12), value: restRadius)
    }

    /// HStack geometry: 13pt fixed hit slots at 2pt spacing.
    private static func slotCenterX(_ index: Int) -> CGFloat {
        6.5 + CGFloat(index) * 15
    }

    /// The blob's fill while the pager is live: the scoped and peeked
    /// workspaces' swatches blended with the SAME trailing curve as the
    /// chrome wash (`AppTintAnimator.scrubWash`: 0.6·progress², so the
    /// goo's color lags slightly behind the fingers and sits ~60% blended
    /// at the commit point), then completed by `release` — a commit's
    /// collapse animates release → 1, carrying the remaining blend to the
    /// full target tint as the blob lands, so the post-select recolor to
    /// the new active swatch renders identically (no flicker). A cancel
    /// springs stretch home and the color follows.
    private static func scrubbedTint(
        active: Color,
        target: Color?,
        pager: SidebarWorkspacePager.DotsPager?
    ) -> Color {
        guard let target, let pager else { return active }
        let stretch = min(1, max(0, pager.stretch))
        let trailing = 0.6 * stretch * stretch
        let release = min(1, max(0, pager.release))
        let fraction = trailing + (1 - trailing) * release
        return active.blended(with: target, fraction: fraction)
    }

    private func refresh() {
        rows = WorkspaceSwitching.orderedRows(store: store)
    }

    private func tooltip(
        for row: WorkspaceListRowModel,
        selected: Bool,
        attention: Bool
    ) -> String {
        var details: [String] = [row.name]
        switch row.kind {
        case let .local(_, isDefault, isCurrentInstance, isRunning):
            if isCurrentInstance {
                details.append(isDefault ? "Default workspace · This Mac" : "Local workspace · This Mac")
            } else {
                details.append(isRunning ? "Local workspace · Running" : "Local workspace")
            }
        case .paired:
            details.append("Paired remote workspace")
        case .ssh:
            details.append("SSH workspace")
        }
        if attention { details.append("A session needs attention") }
        details.append(selected ? "Current workspace" : "Click to switch")
        details.append("Right-click for workspace menu")
        return details.joined(separator: "\n")
    }
}

private struct WorkspaceDot: View {
    let name: String
    let tooltip: String
    var attention = false
    let select: () -> Void
    let hoverChanged: (Bool) -> Void

    @State private var hovering = false

    var body: some View {
        Button(action: select) {
            // Muted neutral marks for every slot; the scoped workspace's
            // color rides above this row as the gooey indicator blob. A
            // background workspace with a session needing input carries a
            // small orange satellite (Theme.attention, same hue as the
            // sidebar's attention dot).
            Circle()
                .fill(Theme.mutedForeground.opacity(0.35))
                .frame(width: 5.5, height: 5.5)
                .overlay(alignment: .topTrailing) {
                    if attention {
                        Circle()
                            .fill(Theme.attention)
                            .frame(width: 4, height: 4)
                            .offset(x: 2, y: -2)
                    }
                }
                .scaleEffect(hovering ? 1.25 : 1)
                // Fixed hit target: hover growth never reflows neighbors,
                // and the full 22pt footer height stays clickable.
                .frame(width: 13, height: 22)
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .onHover { inside in
            hovering = inside
            hoverChanged(inside)
        }
        .help(tooltip)
        .accessibilityLabel("Switch to workspace \(name)")
        .animation(.easeInOut(duration: 0.12), value: hovering)
    }
}

/// The gooey pager blob: a capsule "worm" between the head and tail endpoint
/// coordinates the caller resolves (`gooeyIndicator`). Deliberately animates
/// ONLY plain endpoints + radius — every animated move is a straight line,
/// so no combination of re-based progress values can ever bend the head
/// backward (the old (fromX, stretch) parameterization did exactly that
/// during a commit's post-select re-base, reading as a bounce). Head and
/// tail coincident renders the at-rest active dot: a plain `radius` circle.
/// A capsule morph was chosen over a Canvas blur + alphaThreshold metaball:
/// at this 5.5–7pt dot scale the thresholded edges alias and the neck
/// parameters fight the tiny radii, while the capsule stays crisp and fully
/// `Animatable`.
private struct GooeyDotIndicator: Shape {
    /// Leading-edge slot coordinate of the blob's head (the end that walks
    /// toward the target with the fingers).
    var headX: CGFloat
    /// The tail's slot coordinate; a commit animates it forward onto the
    /// target while the head holds.
    var tailX: CGFloat
    /// Half-height of the capsule (goo-conservation thinning included by
    /// the caller).
    var radius: CGFloat

    var animatableData: AnimatablePair<AnimatablePair<CGFloat, CGFloat>, CGFloat> {
        get {
            AnimatablePair(AnimatablePair(headX, tailX), radius)
        }
        set {
            headX = newValue.first.first
            tailX = newValue.first.second
            radius = newValue.second
        }
    }

    func path(in rect: CGRect) -> Path {
        let minX = min(headX, tailX) - radius
        let maxX = max(headX, tailX) + radius
        return Path(
            roundedRect: CGRect(
                x: minX,
                y: rect.midY - radius,
                width: maxX - minX,
                height: radius * 2
            ),
            cornerRadius: radius
        )
    }
}

// MARK: - Pager math (pure)

/// The pager's pure decision rules, extracted for testability: nothing in
/// here touches views, the store, or global state.
enum SidebarPagerMath {
    /// Near-1:1 finger tracking for small travel, saturating smoothly at
    /// `cap` (tanh rubber band).
    static func rubberBand(_ x: CGFloat, cap: CGFloat) -> CGFloat {
        guard cap > 0, x != 0 else { return 0 }
        return (x < 0 ? -1 : 1) * cap * CGFloat(tanh(Double(abs(x) / cap)))
    }

    /// The neighbor slot a drag in `direction` reveals from `scopedIndex`,
    /// wrapping at both ends. `wraps` = the modular step crossed an end of
    /// the row (the dots blob must not stretch across the whole row).
    static func neighborIndex(
        scopedIndex: Int,
        direction: Int,
        count: Int
    ) -> (index: Int, wraps: Bool) {
        let rawIndex = scopedIndex + direction
        let index = ((rawIndex % count) + count) % count
        return (index, rawIndex != index)
    }

    /// Gesture-end commit rule: ~1/3 of the sidebar width of raw finger
    /// travel commits, as does a decisive flick pointing the same way as
    /// the travel (no bounce-back commits).
    static func shouldCommit(
        travel: CGFloat,
        velocity: CGFloat,
        width: CGFloat,
        flickVelocity: CGFloat,
        flickMinTravel: CGFloat
    ) -> Bool {
        let distance = abs(travel)
        guard distance > 0 else { return false }
        if distance >= width / 3 { return true }
        return distance >= flickMinTravel
            && abs(velocity) >= flickVelocity
            && (velocity < 0) == (travel < 0)
    }

    /// Click/selector slide planning: the target's page enters from the
    /// side it sits on in the shared order, taking the shorter modular
    /// direction (equidistant wrap ties go forward); `isWrap` marks a step
    /// around an end of the row.
    static func slidePlan(
        currentIndex: Int,
        targetIndex: Int,
        count: Int
    ) -> (direction: Int, isWrap: Bool) {
        let forward = ((targetIndex - currentIndex) % count + count) % count
        let backward = ((currentIndex - targetIndex) % count + count) % count
        let direction = forward <= backward ? 1 : -1
        let isWrap = direction == 1
            ? targetIndex < currentIndex
            : targetIndex > currentIndex
        return (direction, isWrap)
    }

    /// THE carousel's page x-position math — the only thing translation is
    /// allowed to change. The live page rides the container translation 1:1;
    /// the neighbor page is parked exactly one page width beyond the edge it
    /// enters from (+1 = next, beyond the trailing edge; -1 = previous,
    /// beyond the leading edge) and rides the same translation. Wrap steps
    /// use the identical math — a wrap only affects the dots blob, never
    /// page geometry.
    static func pageOffsets(
        containerOffset: CGFloat,
        direction: Int,
        width: CGFloat
    ) -> (live: CGFloat, neighbor: CGFloat) {
        (containerOffset, containerOffset + CGFloat(direction) * width)
    }

    /// The container translation a commit animates to: one page over in the
    /// direction of travel, which by `pageOffsets` rests the entering
    /// neighbor page at exactly x = 0.
    static func committedOffset(direction: Int, width: CGFloat) -> CGFloat {
        -CGFloat(direction) * width
    }
}

// MARK: - Carousel state

/// Published state for the workspace carousel — the standard pager model:
/// one container offset, one materialized neighbor page, one
/// pending-commit latch. The heavy sidebar body holds this as plain
/// `@State` — a stable reference with NO subscription — so per-scroll-event
/// `@Published` updates re-run only the tiny observers
/// (`SidebarWorkspaceCarousel` and the footer dots' gooey indicator),
/// never the live project tree body.
///
/// Re-entrancy contract: `begin` (and `programmaticSwitch`) first
/// `settle`s any commit whose slide is still animating — the scope switch
/// lands synchronously, unanimated — and then fully re-bases
/// offset/neighbor/dots to zero. A new gesture therefore always starts
/// from a container at rest; no state accumulates across gestures.
@MainActor
final class SidebarWorkspacePager: ObservableObject {
    struct Neighbor {
        let row: WorkspaceListRowModel
        /// +1 = the NEXT workspace entering from the trailing edge,
        /// -1 = the previous one from the leading edge.
        let direction: Int
    }

    /// Live geometry for the footer dots' gooey indicator. `stretch` is
    /// finger progress toward the neighbor (1 = the commit distance);
    /// `release` walks the blob's tail across after a commit so it
    /// collapses onto the new dot, while a cancelled drag springs
    /// `stretch` home instead.
    struct DotsPager: Equatable {
        let targetID: String
        var stretch: CGFloat
        var release: CGFloat
        /// True when the step wraps around an end of the dot row (previous
        /// from the first workspace / next from the last): the blob keeps
        /// its circle shape on the current dot instead of stretching across
        /// the whole row, and only slides after a commit.
        let isWrap: Bool
    }

    /// The carousel container's horizontal translation. Both pages ride it:
    /// rubber-banded while tracking fingers, animated one page over on
    /// commit, sprung home on cancel. Always exactly 0 at rest.
    @Published private(set) var offset: CGFloat = 0
    /// The materialized neighbor page; nil = single-page rest state.
    @Published private(set) var neighbor: Neighbor?
    /// The footer dots' gooey indicator state; nil = blob at rest.
    @Published private(set) var dotsPager: DotsPager?

    /// True while the carousel owns the list surface: the container is
    /// translated, a neighbor page is materialized, or a commit's swap is
    /// still pending. Row-level interactions whose geometry assumes the
    /// untranslated live page (the detached session drag) gate on this.
    var isTranslated: Bool {
        offset != 0 || neighbor != nil || pendingCommit != nil
    }

    /// Sidebar width captured at gesture start (one page = one width).
    private(set) var width: CGFloat = 0
    /// Translation cap — the container never travels further than ~40% of
    /// the sidebar width while tracking fingers.
    private(set) var cap: CGFloat = 0
    /// Continuously observed sidebar list width (SidebarView's geometry
    /// observer). Programmatic click-switches have no gesture to capture a
    /// width from, so they start from this.
    private(set) var lastKnownWidth: CGFloat = 0

    func noteWidth(_ width: CGFloat) {
        guard width > 0 else { return }
        lastKnownWidth = width
    }

    /// A committed swipe whose slide animation is still running: the scope
    /// switch that must land when the target page reaches x=0 (or
    /// immediately, if a newer interaction takes the surface over first).
    /// THE single latch for "a commit is in flight" — there is no other
    /// cross-gesture state.
    private var pendingCommit: (
        row: WorkspaceListRowModel,
        select: (WorkspaceListRowModel) -> Void
    )?
    /// Bumped on every begin/finish/settle so stale scheduled work no-ops.
    private var generation = 0
    /// Chrome wash at drag start: the scrub blends from here toward the
    /// neighbor workspace's color, and a cancelled drag eases back to it.
    private var fromHue: Double?
    private var fromStrength: Double = 1

    /// Gesture start. Settles any in-flight commit FIRST (its scope switch
    /// lands now, unanimated), then fully re-bases the container to zero —
    /// every gesture starts from rest, by construction.
    func begin(width: CGFloat) {
        settle()
        generation += 1
        self.width = width
        cap = width * 0.4
        rebase()
        // Each gesture (or programmatic slide) renders fresh pooled truth;
        // within the gesture the builder's (row, snapshot) key memoizes per
        // event.
        SidebarListContentBuilder.invalidate()
        // Take manual control of the wash for the drag's duration.
        AppTintAnimator.shared.cancel()
        fromHue = Theme.appTintHue
        fromStrength = Theme.appTintStrength
    }

    /// Land a still-animating commit NOW: the scope switch plus container
    /// re-base, in one unanimated transaction — the same swap the commit's
    /// own timer performs, just earlier. Safe to call at any time; no-op
    /// without a pending commit. Anyone about to read "which workspace is
    /// scoped" for a new interaction calls this first.
    func settle() {
        guard let pending = pendingCommit else { return }
        pendingCommit = nil
        generation += 1
        commitSwap(pending)
    }

    /// Per-event tracking update; `travel` is the raw accumulated deltaX
    /// and `commitDistance` mirrors the monitor's commit rule. The dots
    /// blob's head reaches the neighbor exactly where releasing would
    /// switch; the chrome wash deliberately TRAILS the fingers (scrubWash's
    /// curve caps the blend at the commit point) and completes in the
    /// settle after a commit. A direction flip simply re-materializes the
    /// neighbor page on the other edge.
    func update(
        travel: CGFloat,
        neighborRow: WorkspaceListRowModel,
        direction: Int,
        commitDistance: CGFloat,
        wraps: Bool
    ) {
        guard pendingCommit == nil else { return }
        offset = SidebarPagerMath.rubberBand(travel, cap: cap)
        neighbor = Neighbor(row: neighborRow, direction: direction)
        guard commitDistance > 0 else { return }
        let progress = abs(travel) / commitDistance
        dotsPager = DotsPager(
            targetID: neighborRow.id,
            stretch: min(1, progress),
            release: 0,
            isWrap: wraps
        )
        AppTintAnimator.shared.scrubWash(
            fromHue: fromHue,
            fromStrength: fromStrength,
            toHue: neighborRow.tint.hue,
            progress: progress
        )
    }

    /// Single-slide switch for footer-dot / selector / settings clicks:
    /// materialize the target's pooled page one width beyond the entering
    /// edge and run the exact commit path of a swipe (container slides one
    /// page over, blob morphs to the target dot, scope switches in the
    /// re-basing transaction). Returns false when the carousel cannot run —
    /// a live gesture or cancel spring owns the surface, or no width
    /// observed yet — and the caller switches instantly instead.
    func programmaticSwitch(
        to row: WorkspaceListRowModel,
        direction: Int,
        isWrap: Bool,
        select: @escaping (WorkspaceListRowModel) -> Void
    ) -> Bool {
        guard !SidebarMotion.reduceMotion, lastKnownWidth > 0 else {
            return false
        }
        settle()
        guard neighbor == nil else { return false }
        begin(width: lastKnownWidth)
        neighbor = Neighbor(row: row, direction: direction)
        dotsPager = DotsPager(
            targetID: row.id,
            stretch: 0,
            release: 0,
            isWrap: isWrap
        )
        finish(commit: true, select: select)
        return true
    }

    /// Gesture end. Commit animates the container one page over — the
    /// pooled target page lands at x=0 showing its final pixels — and then
    /// the swap (`commitSwap`) performs the scope switch and re-base in one
    /// unanimated transaction. Cancel springs the container home and eases
    /// the wash back.
    func finish(commit: Bool, select: ((WorkspaceListRowModel) -> Void)?) {
        guard pendingCommit == nil else { return }
        guard let neighbor else {
            rebase()
            return
        }
        generation += 1
        let gen = generation
        if commit, let select {
            pendingCommit = (neighbor.row, select)
            // Keep the color MOVING through the slide: the scrub left the
            // wash at ~60% blend, and the swap (applyScopeTint's own 0.5s
            // settle) only starts after the 200ms commit timer — previously
            // the wash froze for that gap, a visible stall in an otherwise
            // continuous fade. applyScopeTint re-bases from wherever this
            // animation has reached, so the handoff is seamless (and it
            // corrects course if the scoped Host resolves a different hue).
            AppTintAnimator.shared.animate(
                toHue: neighbor.row.tint.hue, duration: 0.6
            )
            withAnimation(.easeOut(duration: 0.18)) {
                offset = SidebarPagerMath.committedOffset(
                    direction: neighbor.direction,
                    width: width
                )
                // The blob's tail lets go and the whole worm collapses onto
                // the new dot.
                if var pager = dotsPager {
                    pager.stretch = 1
                    pager.release = 1
                    dotsPager = pager
                }
            }
            // The slide runs 180ms; the swap waits ~one extra frame so the
            // landed page is committed and drawn before the scope switch
            // replaces it with identical pixels. A newer interaction that
            // needs the surface first settles this commit itself
            // (`settle`), and the generation gate keeps this late timer
            // from double-firing after it has.
            DispatchQueue.main.asyncAfter(
                deadline: .now() + .milliseconds(200)
            ) { [weak self] in
                guard let self, self.generation == gen else { return }
                self.settle()
            }
        } else {
            // Ease the wash back to where the drag started.
            AppTintAnimator.shared.animate(toHue: fromHue, duration: 0.22)
            withAnimation(.spring(response: 0.3, dampingFraction: 0.85)) {
                offset = 0
                // The blob springs back onto the still-scoped dot.
                if var pager = dotsPager {
                    pager.stretch = 0
                    dotsPager = pager
                }
            }
            // The neighbor page stays materialized through the spring (it
            // is visible until the container is home), then dematerializes
            // unanimated. A newer begin re-bases immediately instead.
            DispatchQueue.main.asyncAfter(
                deadline: .now() + .milliseconds(320)
            ) { [weak self] in
                guard let self, self.generation == gen else { return }
                self.rebase()
            }
        }
    }

    /// THE zero-seam swap: scope switch and container re-base in ONE
    /// unanimated transaction. When it runs, the pooled target page sits at
    /// x=0 showing its final pixels; inside the transaction the scope
    /// switch seeds the runtime with the exact pooled snapshot that page
    /// rendered from, the container re-bases to zero, and the neighbor page
    /// dematerializes — so the live page re-renders at x=0 with the same
    /// rows, from the top (SidebarView's scope-change scroll cue lands the
    /// tree at the top inside this same commit). Identical inputs on both
    /// pages by construction is what makes the swap invisible: no cover, no
    /// freeze, no opacity, no second timer. applyScopeTint inside `select`
    /// continues the wash from the scrubbed mid-blend to the destination's
    /// real color, and the dots blob's endpoints already coincide on the
    /// target slot, so clearing it renders identically too.
    private func commitSwap(
        _ pending: (row: WorkspaceListRowModel, select: (WorkspaceListRowModel) -> Void)
    ) {
        var transaction = Transaction()
        transaction.disablesAnimations = true
        withTransaction(transaction) {
            pending.select(pending.row)
            offset = 0
            neighbor = nil
            dotsPager = nil
        }
        // Drop cached pooled content; the next gesture renders fresh truth.
        SidebarListContentBuilder.invalidate()
    }

    /// Container to rest, unanimated: offset zero, no neighbor page, blob
    /// home. Never touches scope.
    private func rebase() {
        var transaction = Transaction()
        transaction.disablesAnimations = true
        withTransaction(transaction) {
            offset = 0
            neighbor = nil
            dotsPager = nil
        }
    }
}

// MARK: - Carousel container

/// ONE carousel container owning the sidebar list area: two materialized
/// pages of the same list component translating together with the shared
/// container offset — the live page at `offset`, the pooled neighbor parked
/// exactly one page width beyond the edge it enters from
/// (`SidebarPagerMath.pageOffsets`). A tiny ViewModifier so
/// per-scroll-event updates re-run only this body: the heavy live tree
/// arrives as an unchanged content proxy and is never re-diffed per event.
/// The neighbor page never hit-tests — hover chrome, context menus, and row
/// verbs stay dormant until it becomes the live page.
///
/// GEOMETRY CONTRACT (do not weaken any leg of it):
///
/// - The carousel is a FIXED-FRAME region exactly the list area's size:
///   `GeometryReader` reports the proposed size REGARDLESS of how tall a
///   page's content is, so nothing a page renders can ever change the
///   region's size — the sidebar VStack (and therefore the footer/dots
///   chrome below, which are siblings OUTSIDE this region and read no pager
///   state) is untouchable by pager activity.
/// - Both pages are absolutely positioned within the region with IDENTICAL
///   explicit frames: region width × region height, TOP-aligned. A pooled
///   page whose snapshot rows are taller than the region overflows past the
///   BOTTOM only (never re-centers itself upward) and is clipped; both
///   pages carry the same `SidebarListPageMetrics.topInset`, so their first
///   rows sit at the same y by construction.
/// - Translation only ever changes the x-offsets of the two pages
///   (`SidebarPagerMath.pageOffsets`); `.offset` is a pure render
///   translation and never participates in layout.
/// - The region clips its contents, so a translated page cannot paint over
///   the sidebar's neighbors.
struct SidebarWorkspaceCarousel: ViewModifier {
    @ObservedObject var pager: SidebarWorkspacePager
    /// Deliberately NOT observed: pooled page content is derived once per
    /// (row, snapshot) via the builder cache, never per store churn.
    let store: UnpeelStore

    func body(content: Content) -> some View {
        GeometryReader { region in
            let size = region.size
            ZStack(alignment: .topLeading) {
                content
                    .frame(width: size.width, height: size.height, alignment: .top)
                    .offset(x: SidebarPagerMath.pageOffsets(
                        containerOffset: pager.offset,
                        direction: pager.neighbor?.direction ?? 1,
                        width: pager.width
                    ).live)
                if let neighbor = pager.neighbor {
                    SidebarListPage(source: .pooled(neighbor.row), store: store)
                        .frame(width: size.width, height: size.height, alignment: .top)
                        .offset(x: SidebarPagerMath.pageOffsets(
                            containerOffset: pager.offset,
                            direction: neighbor.direction,
                            width: pager.width
                        ).neighbor)
                        .allowsHitTesting(false)
                }
            }
            .clipped()
        }
    }
}

/// Shared vertical-geometry contract for BOTH carousel pages. The live tree
/// (SidebarView's `projectTreePanel`) and the pooled page body read the SAME
/// constant, so the two pages cannot diverge on their top inset — if they
/// did, the incoming page would land with its rows at a different y than
/// the live page that replaces it at commit, and the swap would visibly
/// jump.
enum SidebarListPageMetrics {
    /// Top clearance under the fixed titlebar glass. Content offset 0 puts
    /// the first row exactly this far below the region's top edge on both
    /// pages (the pooled page renders from the top by construction; the
    /// live page top-lands on scope change via SidebarView's scroll cue).
    static let topInset: CGFloat = 48
}

/// Which truth a `SidebarListPage` renders from.
enum SidebarListSource {
    /// The scoped workspace: the interactive tree, driven by the live store
    /// exactly as it always was.
    case live
    /// A neighbor workspace: the same rows, read-only, from its pooled
    /// bootstrap snapshot — full-bleed, no card chrome, no identity header,
    /// rendered with the REAL row components
    /// (`ProjectRowView`/`SessionRowView`) so it is indistinguishable from
    /// the live page it becomes on commit (fonts, indentation, status dots,
    /// unread badges, counts, spacing). A blank page with one small muted
    /// spinner appears ONLY when no snapshot exists yet (a never-reached
    /// host); after a commit the connecting state keeps the same blank+
    /// spinner shape, so the whole switch reads as one continuous load.
    case pooled(WorkspaceListRowModel)
}

/// THE sidebar list component both carousel pages materialize. The live
/// page embeds the interactive tree the caller builds (it observes the
/// store itself); the pooled page renders the snapshot projection through
/// the shared builder.
struct SidebarListPage<LiveTree: View>: View {
    let source: SidebarListSource
    /// Not observed here — see the source cases for who reads what.
    let store: UnpeelStore
    @ViewBuilder var liveTree: () -> LiveTree

    var body: some View {
        switch source {
        case .live:
            liveTree()
        case .pooled(let row):
            SidebarPooledListBody(
                content: SidebarListContentBuilder.content(
                    for: row,
                    store: store
                )
            )
        }
    }
}

extension SidebarListPage where LiveTree == EmptyView {
    /// Pooled pages have no live tree to embed.
    init(source: SidebarListSource, store: UnpeelStore) {
        self.init(source: source, store: store) { EmptyView() }
    }
}

/// Read-only sidebar tree for one peeked workspace, carrying exactly the
/// inputs the REAL sidebar row components (`ProjectRowView`/`SessionRowView`)
/// render from — so the panel is pixel-identical to the sidebar the commit
/// scopes into. Grouped like the real list: a top-level project header plus
/// the flattened rows inside it (pinned Sessions first, then the ordinary
/// mixed folder/Session rows), because the real tree lays out top-level items at
/// 1pt spacing and everything inside an expanded project at 0pt.
struct SidebarListContent: Equatable {
    struct FolderRow: Equatable, Identifiable {
        let node: ProjectNode
        let depth: Int
        let isExpanded: Bool
        let showsAttentionDot: Bool
        let showsUnreadDot: Bool
        let showsBusyShimmer: Bool
        let folderColor: ProjectFolderColor?
        var id: String { node.id }
    }

    struct SessionRow: Equatable, Identifiable {
        let entry: SessionEntry
        let depth: Int
        let isPinned: Bool
        /// The session the target workspace will show after the swipe lands.
        /// Painting its glass state in the pooled page prevents the commit
        /// swap from producing a one-pixel-looking active-row jump.
        let isSelected: Bool
        let isUnread: Bool
        let isArchived: Bool
        /// Runtime marks for the pane group carried by this representative row.
        let paneItems: [UnpeelStore.PaneSidebarItem]
        /// Non-nil in date-sorted groups, matching the real row's age source.
        let ageTimestampMs: Int64?
        var id: String { entry.id }
    }

    enum InnerRow: Equatable, Identifiable {
        case folder(FolderRow)
        case session(SessionRow)
        /// The muted "No sessions yet." / "No active sessions." row an
        /// expanded empty project shows.
        case emptyPlaceholder(projectID: String, label: String, depth: Int)

        var id: String {
            switch self {
            case .folder(let row): return "folder:\(row.id)"
            case .session(let row): return "session:\(row.id)"
            case .emptyPlaceholder(let projectID, _, _): return "empty:\(projectID)"
            }
        }
    }

    struct Group: Equatable, Identifiable {
        let header: FolderRow
        /// Rows inside the expanded header; empty while collapsed.
        let rows: [InnerRow]
        var id: String { header.id }
    }

    let groups: [Group]
}

/// Pane-group projection for a carousel page. Pane layouts belong to this
/// Controller window and are scoped independently from Host bootstrap data,
/// so an incoming workspace page must peek the target `pane-layouts.json`
/// slot instead of using the optional pane summary advertised by that Host.
/// Reconciliation happens on a copy: stale/missing members dissolve the
/// preview group and fail open to ordinary Session rows without mutating the
/// target scope before the swipe commits.
@MainActor
struct SidebarPaneProjection {
    let hiddenSessionIDs: Set<String>
    let itemsByRepresentative: [String: [UnpeelStore.PaneSidebarItem]]
    let representativeBySessionID: [String: String]

    init(
        state: PaneLayoutState,
        sessionsByID: [String: SessionEntry],
        eligibleSessionIDs: Set<String>
    ) {
        var reconciled = state
        _ = reconciled.reconcile(
            eligibleSessionIDs: eligibleSessionIDs.intersection(sessionsByID.keys)
        )

        var hidden = Set<String>()
        var items: [String: [UnpeelStore.PaneSidebarItem]] = [:]
        var representatives: [String: String] = [:]
        for group in reconciled.groups {
            guard let representativeID = group.representativeSessionID else {
                continue
            }
            let paneItems: [UnpeelStore.PaneSidebarItem] = group.panes
                .compactMap { pane in
                    guard let sessionID = pane.content.sessionID,
                          let session = sessionsByID[sessionID]
                    else { return nil }
                    return UnpeelStore.paneSidebarItem(
                        paneID: pane.id,
                        session: session,
                        isRepresentative: pane.id == group.representativePaneID
                    )
                }
            guard !paneItems.isEmpty else { continue }
            items[representativeID] = paneItems
            for sessionID in group.sessionIDs {
                representatives[sessionID] = representativeID
                if sessionID != representativeID {
                    hidden.insert(sessionID)
                }
            }
        }
        hiddenSessionIDs = hidden
        itemsByRepresentative = items
        representativeBySessionID = representatives
    }

    func representativeSessionID(for sessionID: String?) -> String? {
        guard let sessionID else { return nil }
        return representativeBySessionID[sessionID] ?? sessionID
    }
}

/// Builds (and memoizes per neighbor row + snapshot) the pooled page tree.
/// The current instance's own row reads local truth (`store.nodes` through
/// the same rendered-list accessors the real sidebar uses); every other row
/// reads its pooled bootstrap snapshot. Nil = no snapshot yet → the blank
/// loading placeholder.
/// No freeze/pinning exists: the commit swap removes the pooled page in the
/// SAME transaction the scope switch lends its snapshot to the runtime, so
/// the page can never re-render against a missing snapshot.
@MainActor
enum SidebarListContentBuilder {
    /// Keep the pooled list bounded: it is a glance, not a scroll surface.
    private static let maximumRows = 40

    private static var cacheKey: String?
    private static var cached: SidebarListContent?

    /// Called at gesture/slide begin so each gesture renders fresh truth;
    /// within one gesture the (row, snapshot) key keeps per-scroll-event
    /// work at a dictionary hit.
    static func invalidate() {
        cacheKey = nil
        cached = nil
    }

    static func content(
        for row: WorkspaceListRowModel,
        store: UnpeelStore
    ) -> SidebarListContent? {
        let key: String
        let content: SidebarListContent?
        if isCurrentInstance(row) {
            // As a Host client, Local truth is the worker's snapshot — the
            // pool mirrors it under this instance's own key while the
            // runtime serves Local. Render the page from that snapshot, the
            // same rows the live tree shows after the commit re-seeds from
            // it. The Swift scan tree is frozen at launch in that mode and
            // would omit every Session created since (the ones inside
            // groups were the visible casualty), making them pop in late.
            let hostSnapshot = currentInstanceRendersHostSnapshot(
                localHostServed: store.localScopeIsHostServed,
                pooledSnapshotAvailable: store.workspacePool.snapshot(forKey: row.id) != nil
            ) ? store.workspacePool.snapshot(forKey: row.id) : nil
            key = "current:\(row.id):\(hostSnapshot?.capturedAtUnixMs ?? 0)"
            if cacheKey == key { return cached }
            content = hostSnapshot.map {
                snapshotContent($0, row: row, store: store, isLocalScope: true)
            } ?? localContent(store: store)
        } else if let snapshot = store.workspacePool.snapshot(forKey: row.id) {
            key = "\(row.id):\(snapshot.capturedAtUnixMs)"
            if cacheKey == key { return cached }
            content = snapshotContent(snapshot, row: row, store: store)
        } else {
            invalidate()
            return nil
        }
        cacheKey = key
        cached = content
        return content
    }

    /// Which truth the current instance's own Local page renders from: the
    /// worker snapshot whenever Local is a Host client and the pool holds
    /// its mirror; the Swift scan tree only for a compatibility (in-app
    /// Host) Local or before the first mirror exists.
    nonisolated static func currentInstanceRendersHostSnapshot(
        localHostServed: Bool,
        pooledSnapshotAvailable: Bool
    ) -> Bool {
        localHostServed && pooledSnapshotAvailable
    }

    private static func isCurrentInstance(_ row: WorkspaceListRowModel) -> Bool {
        if case let .local(_, _, isCurrentInstance, _) = row.kind {
            return isCurrentInstance
        }
        return false
    }

    // MARK: Shared row assembly

    /// Rollups copied from ProjectNodeView: attention > unread on the
    /// collapsed folder mark; busy shimmer over own sessions always plus
    /// worktree descendants while collapsed, suppressed by attention.
    private static func folderRow(
        _ node: ProjectNode,
        depth: Int,
        isExpanded: Bool,
        isUnread: (String) -> Bool,
        folderColor: ProjectFolderColor?
    ) -> SidebarListContent.FolderRow {
        func attention(_ n: ProjectNode) -> Bool {
            n.sessions.contains { $0.status == .attention }
                || n.worktrees.contains(where: attention)
        }
        func unread(_ n: ProjectNode) -> Bool {
            n.sessions.contains { isUnread($0.id) }
                || n.worktrees.contains(where: unread)
        }
        func busy(_ n: ProjectNode) -> Bool {
            n.sessions.contains { $0.status == .starting || $0.status == .busy }
                || n.worktrees.contains(where: busy)
        }
        let hasAttention = attention(node)
        let busyNow = node.sessions.contains { $0.status == .starting || $0.status == .busy }
            || (!isExpanded && node.worktrees.contains(where: busy))
        return SidebarListContent.FolderRow(
            node: node,
            depth: depth,
            isExpanded: isExpanded,
            showsAttentionDot: !isExpanded && hasAttention,
            showsUnreadDot: !isExpanded && unread(node),
            showsBusyShimmer: !hasAttention && busyNow,
            folderColor: folderColor
        )
    }

    /// One project's flattened inner rows, in the real sessionList's order:
    /// the mixed pinned partition first (pinned sessions and pinned child
    /// groups interleaved), then the regular section's interleaved Session
    /// and child-folder rows (recursing when a folder is expanded), then the
    /// empty placeholder. `budget` bounds total row production.
    private static func innerRows(
        for node: ProjectNode,
        depth: Int,
        budget: inout Int,
        hiddenSessionIDs: Set<String>,
        isExpanded: (String) -> Bool,
        folder: (ProjectNode, Int, Bool) -> SidebarListContent.FolderRow,
        pinnedItems: (ProjectNode) -> [SidebarMixedItem],
        sessionLists: (ProjectNode) -> (pinned: [SessionEntry], displayed: [SessionEntry]),
        displayedItems: (ProjectNode, [SessionEntry]) -> [SidebarMixedItem],
        sessionRow: (SessionEntry, Int, Bool) -> SidebarListContent.SessionRow,
        emptyLabel: (ProjectNode) -> String
    ) -> [SidebarListContent.InnerRow] {
        var rows: [SidebarListContent.InnerRow] = []
        func appendItems(_ items: [SidebarMixedItem], pinned: Bool) -> Bool {
            for item in items {
                if case .session(let entry) = item,
                   hiddenSessionIDs.contains(entry.id) {
                    continue
                }
                guard budget > 0 else { return false }
                budget -= 1
                switch item {
                case .session(let entry):
                    rows.append(.session(sessionRow(entry, depth, pinned)))
                case .folder(let child):
                    let expanded = isExpanded(child.id)
                    rows.append(.folder(folder(child, depth + 1, expanded)))
                    if expanded {
                        rows += innerRows(
                            for: child,
                            depth: depth + 1,
                            budget: &budget,
                            hiddenSessionIDs: hiddenSessionIDs,
                            isExpanded: isExpanded,
                            folder: folder,
                            pinnedItems: pinnedItems,
                            sessionLists: sessionLists,
                            displayedItems: displayedItems,
                            sessionRow: sessionRow,
                            emptyLabel: emptyLabel
                        )
                    }
                }
            }
            return true
        }
        let pinned = pinnedItems(node)
        guard appendItems(pinned, pinned: true) else { return rows }
        let regularItems = displayedItems(node, sessionLists(node).displayed)
        guard appendItems(regularItems, pinned: false) else { return rows }
        if pinned.isEmpty, regularItems.isEmpty {
            rows.append(.emptyPlaceholder(
                projectID: node.id,
                label: emptyLabel(node),
                depth: depth
            ))
        }
        return rows
    }

    // MARK: Current instance (local truth)

    /// Swiping back toward this instance's own Local scope: its tree is
    /// already resident — render local truth through the SAME rendered-list
    /// accessors the real sidebar reads, not a pool snapshot.
    private static func localContent(store: UnpeelStore) -> SidebarListContent {
        // While scoped remote, `store.expandedProjectIDs` holds the remote
        // Host's expansion set; the Local one lives under the base key.
        let expanded: Set<String> = store.selectedHostScope == .local
            ? store.expandedProjectIDs
            : Set(AppDefaults.shared.stringArray(
                forKey: UnpeelStore.expandedProjectsKey
            ) ?? [])
        let eligibleSessionIDs: Set<String> = Set(
            store.sessionsByID.values.compactMap { session -> String? in
                guard !store.archivedSessionIDs.contains(session.id),
                      session.isAttachable || session.status == .starting
                else { return nil }
                return session.id
            }
        )
        let paneProjection = SidebarPaneProjection(
            state: store.paneLayoutController.state(forScopeID: "local"),
            sessionsByID: store.sessionsByID,
            eligibleSessionIDs: eligibleSessionIDs
        )
        let selectedSessionID = paneProjection.representativeSessionID(
            for: store.sidebarCarouselLocalSelectedSessionID
        )
        var budget = maximumRows
        var groups: [SidebarListContent.Group] = []
        for node in store.nodes {
            guard budget > 0 else { break }
            budget -= 1
            let isOpen = expanded.contains(node.id)
            let header = folderRow(
                node,
                depth: 0,
                isExpanded: isOpen,
                isUnread: { store.sessionIsUnread($0) },
                folderColor: store.projectFolderColor(for: node.id)
            )
            var rows: [SidebarListContent.InnerRow] = []
            if isOpen {
                rows = innerRows(
                    for: node,
                    depth: 0,
                    budget: &budget,
                    hiddenSessionIDs: paneProjection.hiddenSessionIDs,
                    isExpanded: { expanded.contains($0) },
                    folder: { child, depth, childOpen in
                        folderRow(
                            child,
                            depth: depth,
                            isExpanded: childOpen,
                            isUnread: { store.sessionIsUnread($0) },
                            folderColor: store.projectFolderColor(for: child.id)
                        )
                    },
                    pinnedItems: { n in
                        store.renderedPinnedItems(in: n)
                    },
                    sessionLists: { n in
                        (store.renderedPinnedSessions(in: n),
                         store.renderedDisplayedSessions(in: n))
                    },
                    displayedItems: { n, _ in
                        store.renderedDisplayedItems(in: n)
                    },
                    sessionRow: { entry, depth, pinned in
                        SidebarListContent.SessionRow(
                            entry: entry,
                            depth: depth,
                            isPinned: pinned,
                            isSelected: entry.id == selectedSessionID,
                            isUnread: store.sessionIsUnread(entry.id),
                            isArchived: store.sessionIsRecentArchived(entry.id),
                            paneItems: paneProjection.itemsByRepresentative[entry.id] ?? [],
                            ageTimestampMs: store.isDateSorted(projectID: entry.projectID)
                                ? max(entry.createdAt, entry.lifecycleAtMs ?? 0)
                                : nil
                        )
                    },
                    emptyLabel: { n in
                        store.archivedSessionCount(in: n) == 0
                            ? "No sessions yet."
                            : "No active sessions."
                    }
                )
            }
            groups.append(SidebarListContent.Group(header: header, rows: rows))
        }
        return SidebarListContent(groups: groups)
    }

    // MARK: Pooled snapshot (any other workspace)

    /// A neighbor workspace's pooled bootstrap, projected with the same tree
    /// shape and session-list rules `projectRemoteScope` applies when that
    /// workspace is actually scoped (minus in-flight drag previews/holds,
    /// which cannot exist for a background workspace): Host row order is the
    /// committed order, pins are Host flags, live rows precede the stopped
    /// preview windowed to the user's stopped limit plus unread rows.
    private static func snapshotContent(
        _ snapshot: RemoteBootstrapSnapshot,
        row: WorkspaceListRowModel,
        store: UnpeelStore,
        /// The current instance's own Local scope rendered from its worker
        /// snapshot: expansion lives under the base key, the selection is
        /// the one the live tree restores on return, and Controller-local
        /// folder tints apply (they are this Mac's own overlay).
        isLocalScope: Bool = false
    ) -> SidebarListContent {
        var projects: [Project] = []
        var projectSummaries: [String: RemoteProjectSummary] = [:]
        for (index, summary) in snapshot.projects.enumerated() {
            projects.append(Project(
                id: summary.id,
                name: summary.name,
                path: summary.path,
                pinnedAt: summary.pinned == true ? 1 : nil,
                parentProjectID: summary.parentProjectID,
                sortOrder: index,
                isFolder: summary.isGroup == true ? true : nil,
                worktreeBranch: summary.worktreeBranch,
                workspacesEnabled: nil,
                mcpBlocked: summary.mcpBlocked ? true : nil
            ))
            projectSummaries[summary.id] = summary
        }
        let knownProjectIDs = Set(projects.map(\.id))

        // The per-project "Sidebar" storage group never renders on the desktop
        // (its members live in the right panel, `visibleChildFolders`), so the
        // swipe ghost must drop it too — otherwise a stray "Sidebar" folder
        // appears only in the carousel snapshot.
        let sidebarGroupIDs: Set<String> = Set(
            snapshot.projects.compactMap { summary in
                guard summary.isGroup == true,
                      let parent = summary.parentProjectID,
                      summary.id == UnpeelStore.projectSidebarGroupID(forRoot: parent)
                else { return nil }
                return summary.id
            }
        )

        var summariesByID: [String: RemoteSessionSummary] = [:]
        var entriesByProject: [String: [SessionEntry]] = [:]
        for summary in snapshot.sessions {
            summariesByID[summary.id] = summary
            if knownProjectIDs.contains(summary.projectID) {
                entriesByProject[summary.projectID, default: []].append(
                    UnpeelStore.sessionEntry(fromRemote: summary)
                )
            }
        }

        var childrenOf: [String: [Project]] = [:]
        var topLevel: [Project] = []
        for project in projects {
            if let parent = project.parentProjectID, knownProjectIDs.contains(parent) {
                childrenOf[parent, default: []].append(project)
            } else {
                topLevel.append(project)
            }
        }
        func node(for project: Project) -> ProjectNode {
            let childProjects = (childrenOf[project.id] ?? [])
                .filter {
                    ($0.worktreeBranch != nil || $0.isFolder == true)
                        && !sidebarGroupIDs.contains($0.id)
                }
                .sorted { ($0.sortOrder ?? 0) < ($1.sortOrder ?? 0) }
            return ProjectNode(
                project: project,
                sessions: entriesByProject[project.id] ?? [],
                worktrees: childProjects.map { node(for: $0) }
            )
        }
        let rootNodes = topLevel
            .sorted { ($0.sortOrder ?? 0) < ($1.sortOrder ?? 0) }
            .map { node(for: $0) }

        // Per-Host persisted expansion, same key derivation as
        // `projectRemoteScope`; a never-scoped Host defaults to every root
        // open (matching that first-visit behavior).
        let hostKey = snapshot.macID ?? Self.fallbackHostKey(for: row)
        let expanded: Set<String>
        if isLocalScope {
            // Same rule as `localContent`: while scoped away,
            // `store.expandedProjectIDs` holds the other scope's set and the
            // Local one lives under the base key.
            expanded = store.selectedHostScope == .local
                ? store.expandedProjectIDs
                : Set(AppDefaults.shared.stringArray(
                    forKey: UnpeelStore.expandedProjectsKey
                ) ?? [])
        } else {
            expanded = AppDefaults.shared.stringArray(
                forKey: UnpeelStore.expandedProjectsKey + "." + hostKey
            ).map(Set.init) ?? Set(rootNodes.map(\.id))
        }

        let recency: (SessionEntry) -> Int64 = {
            max($0.createdAt, $0.lifecycleAtMs ?? 0)
        }
        // A still-warm runtime preserves its explicit selection when the
        // user swipes back. A pooled/cold target shows the scope's
        // REMEMBERED selection (what the committed switch will restore) so
        // the preview never flashes the default top row; only a scope with
        // no memory falls back to the runtime's normal default.
        let rememberedSelection = store
            .rememberedScopeSelection(paneScopeID: paneScopeID(for: row))
            .flatMap { id in snapshot.sessions.contains { $0.id == id } ? id : nil }
        let selectedSessionID: String?
        if isLocalScope {
            // The live Local tree restores exactly this id on return.
            selectedSessionID = store.sidebarCarouselLocalSelectedSessionID
        } else if store.runtimeServedWorkspaceKeyAndName()?.key == row.id {
            selectedSessionID = store.remoteHostRuntime.selectedSessionID
        } else {
            selectedSessionID = rememberedSelection
                ?? RemoteHostRuntime.defaultSessionID(in: snapshot)
        }
        let sessionsByID = entriesByProject.values
            .flatMap { $0 }
            .reduce(into: [String: SessionEntry]()) { result, entry in
                result[entry.id] = entry
            }
        let eligibleSessionIDs: Set<String> = Set(
            sessionsByID.values.compactMap { entry -> String? in
                guard summariesByID[entry.id]?.archived != true,
                      entry.isAttachable || entry.status == .starting
                else { return nil }
                return entry.id
            }
        )
        let paneProjection = SidebarPaneProjection(
            state: store.paneLayoutController.state(
                forScopeID: paneScopeID(for: row)
            ),
            sessionsByID: sessionsByID,
            eligibleSessionIDs: eligibleSessionIDs
        )
        let displayedSelectedSessionID = paneProjection.representativeSessionID(
            for: selectedSessionID
        )
        // Pins first (Host flags, Host order); live rows next in Host order;
        // then the Host-windowed stopped/archive preview (unread inactive rows
        // always stay visible).
        func sessionLists(_ n: ProjectNode) -> (pinned: [SessionEntry], displayed: [SessionEntry]) {
            let dateSorted = projectSummaries[n.id]?.dateSorted == true
            let pinned = n.sessions.filter { summariesByID[$0.id]?.pinned == true }
            var candidates = n.sessions.filter { summariesByID[$0.id]?.pinned != true }
            if dateSorted {
                candidates.sort { recency($0) > recency($1) }
            }
            let active = candidates.filter(\.isLive)
            let stopped = candidates.filter { !$0.isLive }
                .sorted { recency($0) > recency($1) }
            // The pooled Host snapshot is already archive-windowed by that
            // Host. Keep its exact projection; this Mac's local preference
            // must not reshape another workspace or remote Host.
            let visibleStopped = stopped
            // The stopped/archive section renders in recency order in the real list
            // regardless of the group's manual order (sidebarSessionBlocks
            // appends `sortedStopped` after the active section).
            return (pinned, active + visibleStopped)
        }
        func makeSessionRow(
            _ entry: SessionEntry, depth: Int, pinned: Bool
        ) -> SidebarListContent.SessionRow {
            let summary = summariesByID[entry.id]
            return SidebarListContent.SessionRow(
                entry: entry,
                depth: depth,
                isPinned: pinned,
                isSelected: entry.id == displayedSelectedSessionID,
                isUnread: summary?.unread == true,
                isArchived: summary?.archived == true,
                paneItems: paneProjection.itemsByRepresentative[entry.id] ?? [],
                ageTimestampMs: projectSummaries[entry.projectID]?.dateSorted == true
                    ? recency(entry)
                    : nil
            )
        }
        let isUnread: (String) -> Bool = { summariesByID[$0]?.unread == true }
        // Remote scope never shows Controller-local folder tints; nil keeps
        // parity with the real remote sidebar.
        func makeFolder(
            _ n: ProjectNode, depth: Int, isOpen: Bool
        ) -> SidebarListContent.FolderRow {
            folderRow(
                n,
                depth: depth,
                isExpanded: isOpen,
                isUnread: isUnread,
                folderColor: isLocalScope ? store.projectFolderColor(for: n.id) : nil
            )
        }

        /// The exact mixed regular-section order used by
        /// UnpeelStore.renderedDisplayedItems for a remote projection.
        func displayedItems(
            _ n: ProjectNode,
            displayed: [SessionEntry]
        ) -> [SidebarMixedItem] {
            let folders = n.worktrees.filter {
                store.isExperimentalEnabled(.worktrees)
                    || $0.project.worktreeBranch == nil
            }
            let pinnedGroupIDs = Set(folders.compactMap { folder in
                projectSummaries[folder.id]?.pinned == true ? folder.id : nil
            })
            func pinnedGroupsFirst(_ items: [SidebarMixedItem]) -> [SidebarMixedItem] {
                guard !pinnedGroupIDs.isEmpty else { return items }
                let pinned = items.filter { item in
                    if case .folder(let folder) = item {
                        return pinnedGroupIDs.contains(folder.id)
                    }
                    return false
                }
                return pinned + items.filter { item in
                    if case .folder(let folder) = item {
                        return !pinnedGroupIDs.contains(folder.id)
                    }
                    return true
                }
            }
            guard !folders.isEmpty else { return displayed.map { .session($0) } }
            let dateSorted = projectSummaries[n.id]?.dateSorted == true
            let order = dateSorted ? [] : (projectSummaries[n.id]?.sessionOrder ?? [])
            let folderIDs = Set(folders.map(\.id))
            guard !dateSorted, order.contains(where: { folderIDs.contains($0) }) else {
                return pinnedGroupsFirst(
                    folders.map { .folder($0) } + displayed.map { .session($0) }
                )
            }

            var byID: [String: SidebarMixedItem] = [:]
            for folder in folders { byID[folder.id] = .folder(folder) }
            for session in displayed { byID[session.id] = .session(session) }
            var seen = Set<String>()
            var ranked: [SidebarMixedItem] = []
            for id in order {
                if let item = byID[id], seen.insert(id).inserted {
                    ranked.append(item)
                }
            }
            let unrankedSessions = displayed
                .filter { !seen.contains($0.id) }
                .sorted { $0.createdAt > $1.createdAt }
                .map { SidebarMixedItem.session($0) }
            let unrankedFolders = folders
                .filter { !seen.contains($0.id) }
                .map { SidebarMixedItem.folder($0) }
            return pinnedGroupsFirst(unrankedSessions + unrankedFolders + ranked)
        }

        var budget = maximumRows
        var groups: [SidebarListContent.Group] = []
        for rootNode in rootNodes {
            guard budget > 0 else { break }
            budget -= 1
            let isOpen = expanded.contains(rootNode.id)
            let header = makeFolder(rootNode, depth: 0, isOpen: isOpen)
            var rows: [SidebarListContent.InnerRow] = []
            if isOpen {
                rows = innerRows(
                    for: rootNode,
                    depth: 0,
                    budget: &budget,
                    hiddenSessionIDs: paneProjection.hiddenSessionIDs,
                    isExpanded: { expanded.contains($0) },
                    folder: makeFolder,
                    // Remote pooled snapshots keep the shipped Host-resolved
                    // behavior: pinned sessions only; pinned groups stay in
                    // the regular section's pinned-first block.
                    pinnedItems: { n in
                        sessionLists(n).pinned.map { .session($0) }
                    },
                    sessionLists: sessionLists,
                    displayedItems: displayedItems,
                    sessionRow: makeSessionRow,
                    emptyLabel: { n in
                        (projectSummaries[n.id]?.archivedSessionCount ?? 0) == 0
                            ? "No sessions yet."
                            : "No active sessions."
                    }
                )
            }
            groups.append(SidebarListContent.Group(header: header, rows: rows))
        }
        return SidebarListContent(groups: groups)
    }

    /// Controller pane-scope identity for a workspace-list row. The current
    /// app instance is the distinguished `local` slot; every other row uses
    /// the same stable workspace/Host mapping as `SelectedHostScope`.
    private static func paneScopeID(for row: WorkspaceListRowModel) -> String {
        if isCurrentInstance(row) { return "local" }
        switch row.kind {
        case .local:
            return row.home.map { "workspace:\($0)" } ?? "local"
        case .paired(let host):
            return "host:\(host.hostID)"
        case .ssh(let host):
            return "host:\(host.id)"
        }
    }

    /// Same fallback chain `projectRemoteScope` uses when a bootstrap
    /// carries no macID.
    private static func fallbackHostKey(for row: WorkspaceListRowModel) -> String {
        switch row.kind {
        case .local: return row.home ?? "remote"
        case .paired(let host): return host.hostID
        case .ssh(let host): return host.id
        }
    }
}

/// The pooled page's body: a NORMAL sidebar list built from the REAL
/// `ProjectRowView`/`SessionRowView` components with snapshot-derived,
/// read-only inputs — same fonts, indentation, status/attention dots, unread
/// badges, and spacing as the live page it becomes on commit. No identity
/// header: the chrome tint wash, the footer dots pager,
/// and the green host button after commit carry workspace identity.
/// Interactivity is structurally absent (inert callbacks, empty preset
/// lists, no drag provider) and the carousel never hit-tests this page, so
/// hover chrome and context menus stay dormant.
private struct SidebarPooledListBody: View {
    let content: SidebarListContent?

    var body: some View {
        // Fixed-frame page, matching the carousel's geometry contract: the
        // GeometryReader reports EXACTLY the proposed page size no matter
        // how tall the snapshot rows are, the content pins to the TOP of
        // that frame (a tall snapshot overflows past the bottom only, never
        // re-centers upward), and the overflow is clipped — so the fade
        // mask spans the page, not the content's ideal height, putting the
        // bottom fade at the page's bottom edge exactly like the live
        // ScrollView's.
        GeometryReader { page in
            VStack(alignment: .leading, spacing: 0) {
                if let content {
                    if content.groups.isEmpty {
                        Text("No projects yet")
                            .font(Theme.rowLabelFont)
                            .foregroundStyle(Theme.mutedForeground)
                            .padding(EdgeInsets(
                                top: SidebarListPageMetrics.topInset + 4,
                                leading: 17, bottom: 0, trailing: 16
                            ))
                    } else {
                        // Same container metrics as the real project tree:
                        // 1pt between top-level items, 0pt inside an
                        // expanded project, and the SHARED top inset under
                        // the titlebar glass (SidebarListPageMetrics — one
                        // constant for both carousel pages).
                        LazyVStack(spacing: 1) {
                            ForEach(content.groups) { group in
                                // One project is one outer-stack child, just
                                // like the live ProjectNodeView. Without this
                                // wrapper the outer 1pt project spacing also
                                // landed between the folder header and its
                                // first session, then vanished on commit.
                                VStack(alignment: .leading, spacing: 0) {
                                    folderRow(group.header)
                                    if !group.rows.isEmpty {
                                        LazyVStack(alignment: .leading, spacing: 0) {
                                            ForEach(group.rows) { inner in
                                                innerRow(inner)
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        .padding(EdgeInsets(
                            top: SidebarListPageMetrics.topInset,
                            leading: 8, bottom: 12, trailing: 8
                        ))
                    }
                } else {
                    // Never-reached workspace (no pooled snapshot before
                    // first contact): a blank page with one small muted
                    // spinner.
                    SidebarLoadingPlaceholder()
                }
                Spacer(minLength: 0)
            }
            .frame(
                width: page.size.width,
                height: page.size.height,
                alignment: .topLeading
            )
            .clipped()
            // The rows dissolve into the fixed top chrome exactly like the
            // real list (which masks its ScrollView with the same ramp).
            .mask(SidebarListFadeMask())
        }
    }

    private func folderRow(_ row: SidebarListContent.FolderRow) -> some View {
        ProjectRowView(
            node: row.node,
            depth: row.depth,
            isExpanded: row.isExpanded,
            showsAttentionDot: row.showsAttentionDot,
            showsUnreadDot: row.showsUnreadDot,
            showsBusyShimmer: row.showsBusyShimmer,
            quickGroups: [],
            menuPresets: [],
            folderColor: row.folderColor,
            onToggle: {},
            onLaunchPreset: { _ in },
            isPinned: row.node.project.acceptsSessionDrop
                && row.node.project.pinnedAt != nil
        ).equatable()
    }

    @ViewBuilder
    private func innerRow(_ inner: SidebarListContent.InnerRow) -> some View {
        switch inner {
        case .folder(let row):
            folderRow(row)
        case .session(let row):
            SessionRowView(
                session: row.entry,
                depth: row.depth,
                indentBase: 9,
                isSelected: row.isSelected,
                isUnread: row.isUnread,
                isPinned: row.isPinned,
                canPin: false,
                isArchived: row.isArchived,
                canClearAttention: false,
                paneItems: row.paneItems,
                ageTimestampMs: row.ageTimestampMs,
                onSelect: {}
            ).equatable()
        case .emptyPlaceholder(_, let label, let depth):
            // The REAL component with inert inputs (the page never
            // hit-tests), so the label's Menu-chrome metrics are identical
            // to the live sidebar and the snap cannot restyle it.
            EmptySessionsPlaceholderRow(
                label: label,
                depth: depth,
                menuPresets: [],
                onLaunch: { _ in },
                showsManagePresets: false
            )
        }
    }
}

// MARK: - Trackpad swipe

/// Invisible NSView sized to the sidebar that watches local scrollWheel
/// events (same local-monitor pattern as RemoveConfirmDismissMonitor). While
/// the cursor is over the sidebar, a horizontal-dominant two-finger gesture
/// engages the carousel: rows are cached once at engagement (after settling
/// any in-flight commit, so scoped truth is current), and each event just
/// pushes accumulated travel into the pager. Gesture end commits (≥1/3
/// sidebar width of travel, or a decisive same-direction flick) via
/// `select`, otherwise springs back. Momentum-tail events never re-trigger;
/// vertical-dominant gestures never engage. Reduce Motion keeps the
/// original one-shot threshold switch (no translation), and phase-less
/// legacy wheel events keep the instantaneous `cycle` fallback. Natural
/// direction: fingers left → next in list, fingers right → previous.
struct SidebarWorkspaceSwipeMonitor: NSViewRepresentable {
    let state: SidebarWorkspacePager
    /// Built once per gesture at engagement, never per event.
    let rows: () -> [WorkspaceListRowModel]
    let isScoped: (WorkspaceListRowModel) -> Bool
    /// Commit: switch scope to the peeked row (existing tint crossfade).
    let select: (WorkspaceListRowModel) -> Void
    /// Instantaneous fallback (+1 next / -1 previous): Reduce Motion and
    /// phase-less legacy wheel events.
    let cycle: (Int) -> Void

    func makeNSView(context _: Context) -> MonitorView {
        let view = MonitorView()
        apply(to: view)
        return view
    }

    func updateNSView(_ view: MonitorView, context _: Context) {
        apply(to: view)
    }

    private func apply(to view: MonitorView) {
        view.state = state
        view.rows = rows
        view.isScoped = isScoped
        view.select = select
        view.cycle = cycle
    }

    final class MonitorView: NSView {
        var state: SidebarWorkspacePager?
        var rows: (() -> [WorkspaceListRowModel])?
        var isScoped: ((WorkspaceListRowModel) -> Bool)?
        var select: ((WorkspaceListRowModel) -> Void)?
        var cycle: ((Int) -> Void)?

        /// A gesture's fate is decided once, at the slop boundary.
        private enum Mode {
            case undecided
            /// Interactive peek is driving `state`.
            case peek
            /// Reduce Motion: the original one-shot threshold switch.
            case threshold
            /// Vertical-dominant (or otherwise ineligible): inert until end.
            case rejected
        }

        private var monitor: Any?
        private var mode: Mode = .undecided
        private var accumX: CGFloat = 0
        private var accumY: CGFloat = 0
        private var latched = false
        /// Exponentially smoothed finger velocity (pt/s) for flick commits.
        private var velocity: CGFloat = 0
        private var lastEventTime: TimeInterval = 0
        /// Cached at engagement; per-event work stays trivial.
        private var cachedRows: [WorkspaceListRowModel] = []
        private var scopedIndex = 0

        // Legacy phase-less wheel fallback (no gesture lifecycle to track:
        // a quiet gap re-arms the one-shot latch).
        private var legacyAccumX: CGFloat = 0
        private var legacyAccumY: CGFloat = 0
        private var legacyLatched = false
        private var legacyLastTime: TimeInterval = 0

        /// Travel before the gesture's fate (engage vs reject) is decided.
        private static let engageSlop: CGFloat = 8
        /// One swipe = one switch (Reduce Motion + legacy wheel paths).
        private static let cycleThreshold: CGFloat = 60
        /// Flick commit: at least this fast…
        private static let flickVelocity: CGFloat = 500
        /// …with at least this much same-direction travel.
        private static let flickMinTravel: CGFloat = 30

        override func hitTest(_: NSPoint) -> NSView? { nil }

        override func viewDidMoveToWindow() {
            super.viewDidMoveToWindow()
            removeMonitorIfNeeded()
            guard window != nil else {
                // Unmounted mid-gesture (settings opened, gate flipped):
                // never leave the content translated.
                if mode == .peek {
                    state?.finish(commit: false, select: nil)
                }
                mode = .undecided
                return
            }
            monitor = NSEvent.addLocalMonitorForEvents(
                matching: .scrollWheel
            ) { [weak self] event in
                self?.handle(event)
                return event
            }
        }

        private func handle(_ event: NSEvent) {
            guard event.window === window else { return }
            // The momentum tail of a finished gesture must never re-trigger.
            guard event.momentumPhase.isEmpty else { return }
            if event.phase.isEmpty {
                handleLegacy(event)
                return
            }
            switch event.phase {
            case .began:
                mode = .undecided
                accumX = 0
                accumY = 0
                latched = false
                velocity = 0
                lastEventTime = event.timestamp
            case .changed:
                changed(event)
            case .ended, .cancelled:
                if mode == .peek {
                    finishPeek(commit: event.phase == .ended && shouldCommit())
                }
                mode = .undecided
                accumX = 0
                accumY = 0
                latched = false
            default:
                break
            }
        }

        private func changed(_ event: NSEvent) {
            let dt = event.timestamp - lastEventTime
            lastEventTime = event.timestamp
            accumX += event.scrollingDeltaX
            accumY += event.scrollingDeltaY
            if dt > 0 {
                let instantaneous = event.scrollingDeltaX / CGFloat(dt)
                velocity = velocity * 0.7 + instantaneous * 0.3
            }
            switch mode {
            case .rejected:
                return
            case .undecided:
                // Wait for a small slop, then lock the gesture's fate:
                // vertical-dominant list scrolling with sideways drift must
                // never engage (or later switch) workspaces.
                guard abs(accumX) >= Self.engageSlop
                    || abs(accumY) >= Self.engageSlop else { return }
                guard abs(accumX) > abs(accumY),
                      overSidebar(event) else {
                    mode = .rejected
                    return
                }
                if SidebarMotion.reduceMotion {
                    mode = .threshold
                    checkCycleThreshold()
                    return
                }
                guard let state else {
                    mode = .rejected
                    return
                }
                // A commit whose slide is still animating lands NOW, so the
                // scoped index below is computed against settled truth (and
                // the container is guaranteed at rest before `begin`).
                state.settle()
                guard let built = rows?(), built.count > 1 else {
                    mode = .rejected
                    return
                }
                cachedRows = built
                scopedIndex = built.firstIndex { isScoped?($0) ?? false } ?? 0
                mode = .peek
                state.begin(width: bounds.width)
                updatePeek()
            case .threshold:
                checkCycleThreshold()
            case .peek:
                updatePeek()
            }
        }

        private func updatePeek() {
            guard let state else { return }
            // Natural scrolling: fingers moving left report negative deltaX
            // and reveal the NEXT workspace from the trailing edge. Crossing
            // zero mid-gesture flips the neighbor (wrap at the ends).
            let direction = accumX < 0 ? 1 : -1
            let step = SidebarPagerMath.neighborIndex(
                scopedIndex: scopedIndex,
                direction: direction,
                count: cachedRows.count
            )
            state.update(
                travel: accumX,
                neighborRow: cachedRows[step.index],
                direction: direction,
                // Same rule as shouldCommit: the dots blob reaches the
                // neighbor exactly at the commit point, while the tint
                // scrub trails behind (scrubWash's curve) and completes
                // only in the post-switch settle.
                commitDistance: bounds.width / 3,
                // Stepping past either end of the row: the dots blob must
                // not stretch across the whole row.
                wraps: step.wraps
            )
        }

        private func shouldCommit() -> Bool {
            SidebarPagerMath.shouldCommit(
                travel: accumX,
                velocity: velocity,
                width: bounds.width,
                flickVelocity: Self.flickVelocity,
                flickMinTravel: Self.flickMinTravel
            )
        }

        private func finishPeek(commit: Bool) {
            state?.finish(commit: commit, select: commit ? select : nil)
        }

        private func checkCycleThreshold() {
            guard !latched,
                  abs(accumX) >= Self.cycleThreshold,
                  abs(accumX) > abs(accumY)
            else { return }
            latched = true
            cycle?(accumX < 0 ? 1 : -1)
        }

        /// Only while the cursor is over the sidebar (this view spans it).
        private func overSidebar(_ event: NSEvent) -> Bool {
            convert(bounds, to: nil).contains(event.locationInWindow)
        }

        private func handleLegacy(_ event: NSEvent) {
            // Phase-less legacy wheel events (older mice/tablets) keep the
            // instantaneous threshold switch — no lifecycle to track, so a
            // quiet gap re-arms the one-shot latch.
            if event.timestamp - legacyLastTime > 0.3 {
                legacyAccumX = 0
                legacyAccumY = 0
                legacyLatched = false
            }
            legacyLastTime = event.timestamp
            guard overSidebar(event) else { return }
            legacyAccumX += event.scrollingDeltaX
            legacyAccumY += event.scrollingDeltaY
            guard !legacyLatched,
                  abs(legacyAccumX) >= Self.cycleThreshold,
                  abs(legacyAccumX) > abs(legacyAccumY)
            else { return }
            legacyLatched = true
            cycle?(legacyAccumX < 0 ? 1 : -1)
        }

        private func removeMonitorIfNeeded() {
            if let monitor {
                NSEvent.removeMonitor(monitor)
                self.monitor = nil
            }
        }

        deinit {
            // Normally removed in viewDidMoveToWindow(nil) on unmount; this
            // is the safety net. NSViews live on main.
            MainActor.assumeIsolated {
                if let monitor { NSEvent.removeMonitor(monitor) }
            }
        }
    }
}
