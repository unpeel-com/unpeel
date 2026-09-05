//
//  ChromeIcons.swift
//  UnpeelNative
//
//  App-chrome icons (sidebar, titlebar, footer), carried over verbatim from
//  the Svelte app so the native build uses the exact same artwork:
//  - Phosphor-style 256-viewBox fill icons from lib/icons.ts
//  - the Lucide "split" branch icon inlined in ProjectItem.svelte:307-308
//  - the two inline titlebar SVGs in App.svelte:1440/1449
//
//  Same pattern as ToolIcons.swift: `currentColor` is rewritten to #FFFFFF
//  and the decoded NSImage is marked as a template, so SwiftUI's
//  foregroundStyle tints it (hover swaps muted → foreground exactly like
//  the Svelte `color` transitions).
//

import AppKit
import SwiftUI

enum ChromeIcon: String, CaseIterable {
    case folderClosed
    case folderOpen
    case folderSimple
    case branch
    case gitBranch
    case pin
    case pinSlash
    /// Detach a companion from the terminal split.
    case paneUnpin
    case pushPin
    case settings
    case addProjectPlus
    case plus
    case plusBold
    case collapseAll
    case mobile
    case sidebarToggle
    /// Right-panel mark for the project "Sidebar" group row.
    case projectSidebar
    case back
    case bell
    case orchestrator
    case broadcast
    /// Two vertical panes — the sidebar agent mark on a split (merged) session.
    case splitPanes
    /// Phosphor square-split-vertical (line) — pane-header Split Pane (⌘D).
    case splitPane
    /// Its stacked twin (horizontal divider) — pane-header Split Pane Down
    /// (⌘⇧D).
    case splitPaneDown
    /// Phosphor "globe" (line) — local-site links everywhere they surface
    /// (project row, collapsed-sidebar chip, URL menus, the detected toast).
    case globe
    // Settings sidebar tab icons — Phosphor fill glyphs rendered with the
    // same glass gradient as the sidebar folder/gear marks.
    /// Phosphor "circle-half" — Settings ▸ Appearance.
    case settingsAppearance
    /// Phosphor "textbox" — Settings ▸ Presets.
    case settingsPresets
    /// Phosphor "sidebar" — Settings ▸ Workspaces.
    case settingsWorkspaces
    /// Phosphor "monitor + phone" — Settings ▸ Remote Control.
    case settingsRemote
    /// Phosphor "article-medium" — Settings ▸ Transcripts.
    case settingsTranscripts
    /// Phosphor "bell-simple" — Settings ▸ Notifications.
    case settingsNotifications
    /// Phosphor "flask" — Settings ▸ Experimental.
    case settingsExperimental
    /// Phosphor "chats" — Settings ▸ Sessions use.
    case settingsSessions
    /// Phosphor "cursor-click" — Settings ▸ Computer use.
    case settingsComputer
    /// Phosphor "globe" (fill) — Settings ▸ Browser use.
    case settingsBrowser
    /// Phosphor "git-branch" (fill) — Settings ▸ Worktrees.
    case settingsWorktrees
    /// Phosphor "terminal-window" (fill) — Settings ▸ Advanced.
    case settingsAdvanced

    fileprivate func svg(inverted: Bool) -> String {
        switch self {
        // Project row folder mark — collapsed.
        case .folderClosed:
            return ##"<svg xmlns="http://www.w3.org/2000/svg" width="32" height="32" fill="none" viewBox="0 0 256 256"><defs><linearGradient id="folderClosedGlass" x1="36" y1="38" x2="222" y2="218" gradientUnits="userSpaceOnUse">"## + Self.glassStops(inverted: inverted) + ##"</linearGradient></defs><path d="M216,72H131.31L104,44.69A15.88,15.88,0,0,0,92.69,40H40A16,16,0,0,0,24,56V200.62A15.41,15.41,0,0,0,39.39,216h177.5A15.13,15.13,0,0,0,232,200.89V88A16,16,0,0,0,216,72ZM40,56H92.69l16,16H40Z" fill="url(#folderClosedGlass)"></path><path d="M216,72H131.31L104,44.69A15.88,15.88,0,0,0,92.69,40H40A16,16,0,0,0,24,56V200.62A15.41,15.41,0,0,0,39.39,216h177.5A15.13,15.13,0,0,0,232,200.89V88A16,16,0,0,0,216,72ZM40,56H92.69l16,16H40Z" fill="none" stroke="#FFFFFF" stroke-opacity="0.30" stroke-width="8" stroke-linejoin="round"></path></svg>"##
        // Project row folder mark — expanded.
        case .folderOpen:
            return ##"<svg xmlns="http://www.w3.org/2000/svg" width="32" height="32" fill="none" viewBox="0 0 256 256"><defs><linearGradient id="folderOpenGlass" x1="34" y1="46" x2="224" y2="216" gradientUnits="userSpaceOnUse">"## + Self.glassStops(inverted: inverted) + ##"</linearGradient></defs><path d="M245,110.64A16,16,0,0,0,232,104H216V88a16,16,0,0,0-16-16H130.67L102.94,51.2a16.14,16.14,0,0,0-9.6-3.2H40A16,16,0,0,0,24,64V208h0a8,8,0,0,0,8,8H211.1a8,8,0,0,0,7.59-5.47l28.49-85.47A16.05,16.05,0,0,0,245,110.64ZM93.34,64,123.2,86.4A8,8,0,0,0,128,88h72v16H69.77a16,16,0,0,0-15.18,10.94L40,158.7V64Z" fill="url(#folderOpenGlass)"></path><path d="M245,110.64A16,16,0,0,0,232,104H216V88a16,16,0,0,0-16-16H130.67L102.94,51.2a16.14,16.14,0,0,0-9.6-3.2H40A16,16,0,0,0,24,64V208h0a8,8,0,0,0,8,8H211.1a8,8,0,0,0,7.59-5.47l28.49-85.47A16.05,16.05,0,0,0,245,110.64ZM93.34,64,123.2,86.4A8,8,0,0,0,128,88h72v16H69.77a16,16,0,0,0-15.18,10.94L40,158.7V64Z" fill="none" stroke="#FFFFFF" stroke-opacity="0.30" stroke-width="8" stroke-linejoin="round"></path></svg>"##
        // Phosphor "folder" outline — inline worktree child folder rows in
        // the sidebar (lighter than the glass-filled project folder marks).
        case .folderSimple:
            return ##"<svg xmlns="http://www.w3.org/2000/svg" width="32" height="32" fill="none" viewBox="0 0 256 256"><rect width="256" height="256" fill="none"/><path d="M224,88V200.89a7.11,7.11,0,0,1-7.11,7.11H40a8,8,0,0,1-8-8V64a8,8,0,0,1,8-8H93.33a8,8,0,0,1,4.8,1.6L128,80h88A8,8,0,0,1,224,88Z" fill="none" stroke="#FFFFFF" stroke-linecap="round" stroke-linejoin="round" stroke-width="16"/></svg>"##
        // ProjectItem.svelte:307-308 (workspaceBranchIcon): Lucide "split",
        // stroke-width 2. The Svelte string rotates it 90° via an inline
        // style; ChromeIconView applies the same rotation in SwiftUI since
        // NSImage's SVG decoder ignores `style` transforms.
        case .branch:
            return ##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" width="12" height="12" fill="none" stroke="#FFFFFF" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M16 3h5v5"/><path d="M8 3H3v5"/><path d="M12 22v-8.3a4 4 0 0 0-1.172-2.872L3 3"/><path d="m15 9 6-6"/></svg>"##
        // Lucide "git-branch": the standard current-branch mark for a plain
        // (non-worktree) git project's titlebar suffix.
        case .gitBranch:
            return ##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" width="12" height="12" fill="none" stroke="#FFFFFF" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><line x1="6" x2="6" y1="3" y2="15"/><circle cx="18" cy="6" r="3"/><circle cx="6" cy="18" r="3"/><path d="M18 9a9 9 0 0 1-9 9"/></svg>"##
        // icons.ts:29 (pinIcon) — session-row pin action
        case .pin:
            return ##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" fill="#FFFFFF" viewBox="0 0 256 256"><path d="M233.91,82.79,173.22,22.1a14,14,0,0,0-19.81,0L98.93,76.77c-9.52-3.25-34-8.34-59.71,12.41A14,14,0,0,0,38.1,110l49.71,49.71-44.05,44a6,6,0,1,0,8.48,8.48l44.05-44.05L146,217.89a14,14,0,0,0,9.9,4.11q.49,0,1,0a14,14,0,0,0,10.19-5.54c19.72-26.21,17.15-47.23,12.46-59.3l54.37-54.55A14,14,0,0,0,233.91,82.79ZM225.42,94.1h0l-57.27,57.46a6,6,0,0,0-1.11,6.92c9.94,19.88-1.71,40.32-9.54,50.72a2,2,0,0,1-3,.2L46.58,101.51a2,2,0,0,1,.18-3c12.5-10.09,24.5-12.76,33.7-12.76a42.13,42.13,0,0,1,17.25,3.41A6,6,0,0,0,104.64,88L161.9,30.59a2,2,0,0,1,2.83,0l60.69,60.68A2,2,0,0,1,225.42,94.1Z"></path></svg>"##
        // icons.ts:30 (unpinIcon, slashed pin) — pinned session rows
        case .pinSlash:
            return ##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" fill="#FFFFFF" viewBox="0 0 256 256"><path d="M52.44,36A6,6,0,0,0,43.56,44L71.27,74.51C61.78,76,50.6,80,39.22,89.18A14,14,0,0,0,38.1,110l49.71,49.71-44.05,44a6,6,0,1,0,8.48,8.48l44.05-44.05L146,217.89a14,14,0,0,0,9.9,4.11q.49,0,1,0a14,14,0,0,0,10.19-5.54,85.51,85.51,0,0,0,12.44-22.84l24,26.45a6,6,0,1,0,8.87-8.08ZM157.49,209.21a2,2,0,0,1-3,.2L46.58,101.51a2,2,0,0,1,.18-3c13.18-10.64,25.84-12.9,34.79-12.7L170,183.11C167.83,193.74,162.11,203.07,157.49,209.21Zm76.42-106.62-44.65,44.78a6,6,0,1,1-8.5-8.47l44.65-44.79a2,2,0,0,0,0-2.84L164.73,30.59a2,2,0,0,0-2.83,0L120.68,71.94a6,6,0,0,1-8.5-8.47l41.23-41.36a14,14,0,0,1,19.81,0l60.69,60.69A14,14,0,0,1,233.91,102.59Z"></path></svg>"##
        // User-selected pane detach/unpin mark.
        case .paneUnpin:
            return ##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" fill="#FFFFFF" viewBox="0 0 256 256"><rect width="256" height="256" fill="none"/><path d="M232,128a8,8,0,0,1-8,8H152v64a8,8,0,0,1-13.66,5.66l-72-72a8,8,0,0,1,0-11.32l72-72A8,8,0,0,1,152,56v64h72A8,8,0,0,1,232,128ZM40,32a8,8,0,0,0-8,8V216a8,8,0,0,0,16,0V40A8,8,0,0,0,40,32Z"/></svg>"##
        // Phosphor "push-pin"-style thumbtack — session-row pin action while
        // the row is NOT yet pinned (the "click to pin" affordance).
        case .pushPin:
            return ##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" fill="#FFFFFF" viewBox="0 0 256 256"><path d="M216,168h-9.29L185.54,48H192a8,8,0,0,0,0-16H64a8,8,0,0,0,0,16h6.46L49.29,168H40a8,8,0,0,0,0,16h80v56a8,8,0,0,0,16,0V184h80a8,8,0,0,0,0-16ZM86.71,48h82.58l21.17,120H65.54Z"></path></svg>"##
        // Footer settings gear.
        case .settings:
            return Self.glassSVG(
                path: ##"M237.94,107.21a8,8,0,0,0-3.89-5.4l-29.83-17-.12-33.62a8,8,0,0,0-2.83-6.08,111.91,111.91,0,0,0-36.72-20.67,8,8,0,0,0-6.46.59L128,41.85,97.88,25a8,8,0,0,0-6.47-.6A111.92,111.92,0,0,0,54.73,45.15a8,8,0,0,0-2.83,6.07l-.15,33.65-29.83,17a8,8,0,0,0-3.89,5.4,106.47,106.47,0,0,0,0,41.56,8,8,0,0,0,3.89,5.4l29.83,17,.12,33.63a8,8,0,0,0,2.83,6.08,111.91,111.91,0,0,0,36.72,20.67,8,8,0,0,0,6.46-.59L128,214.15,158.12,231a7.91,7.91,0,0,0,3.9,1,8.09,8.09,0,0,0,2.57-.42,112.1,112.1,0,0,0,36.68-20.73,8,8,0,0,0,2.83-6.07l.15-33.65,29.83-17a8,8,0,0,0,3.89-5.4A106.47,106.47,0,0,0,237.94,107.21ZM128,168a40,40,0,1,1,40-40A40,40,0,0,1,128,168Z"##,
                gradientID: "settingsGlass",
                inverted: inverted
            )
        // Footer add-project button.
        case .addProjectPlus:
            return ##"<svg xmlns="http://www.w3.org/2000/svg" width="32" height="32" fill="#FFFFFF" viewBox="0 0 256 256"><path d="M228,128a12,12,0,0,1-12,12H140v76a12,12,0,0,1-24,0V140H40a12,12,0,0,1,0-24h76V40a12,12,0,0,1,24,0v76h76A12,12,0,0,1,228,128Z"></path></svg>"##
        // icons.ts:21 (plusIcon) — footer add-project + quick-strip "+"
        case .plus:
            return ##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" fill="#FFFFFF" viewBox="0 0 256 256"><path d="M222,128a6,6,0,0,1-6,6H134v82a6,6,0,0,1-12,0V134H40a6,6,0,0,1,0-12h82V40a6,6,0,0,1,12,0v82h82A6,6,0,0,1,222,128Z"></path></svg>"##
        // App.svelte:1449 (inline) — collapsed-sidebar "new session" plus
        // (slightly heavier 8-unit strokes than icons.ts plusIcon)
        case .plusBold:
            return ##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" fill="#FFFFFF" viewBox="0 0 256 256"><path d="M224,128a8,8,0,0,1-8,8H136v80a8,8,0,0,1-16,0V136H40a8,8,0,0,1,0-16h80V40a8,8,0,0,1,16,0v80h80A8,8,0,0,1,224,128Z"></path></svg>"##
        // icons.ts:23 (collapseAllIcon) — footer collapse-all
        case .collapseAll:
            return ##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" fill="#FFFFFF" viewBox="0 0 256 256"><path d="M222,128a6,6,0,0,1-6,6H40a6,6,0,0,1,0-12H216A6,6,0,0,1,222,128Zm-98.24-27.76a6,6,0,0,0,8.48,0l32-32a6,6,0,0,0-8.48-8.48L134,81.51V16a6,6,0,0,0-12,0V81.51L100.24,59.76a6,6,0,0,0-8.48,8.48Zm8.48,55.52a6,6,0,0,0-8.48,0l-32,32a6,6,0,0,0,8.48,8.48L122,174.49V240a6,6,0,0,0,12,0V174.49l21.76,21.75a6,6,0,0,0,8.48-8.48Z"></path></svg>"##
        // Phosphor "device-mobile" — retained for phone-specific surfaces.
        case .mobile:
            return Self.glassSVG(
                path: ##"M176,16H80A24,24,0,0,0,56,40V216a24,24,0,0,0,24,24h96a24,24,0,0,0,24-24V40A24,24,0,0,0,176,16Zm8,200a8,8,0,0,1-8,8H80a8,8,0,0,1-8-8V40a8,8,0,0,1,8-8h96a8,8,0,0,1,8,8ZM144,56a8,8,0,0,1-8,8H120a8,8,0,0,1,0-16h16A8,8,0,0,1,144,56Zm-8,136a8,8,0,1,1-8-8A8,8,0,0,1,136,192Z"##,
                gradientID: "mobileGlass",
                inverted: inverted
            )
        // titlebar sidebar toggle.
        case .sidebarToggle:
            return Self.glassSVG(
                path: ##"M216,40H40A16,16,0,0,0,24,56V200a16,16,0,0,0,16,16H216a16,16,0,0,0,16-16V56A16,16,0,0,0,216,40Zm0,160H88V56H216V200Z"##,
                gradientID: "sidebarToggleGlass",
                inverted: inverted
            )
        // Mirror of the sidebar-toggle glyph: panel highlighted on the RIGHT.
        case .projectSidebar:
            return Self.glassSVG(
                path: ##"M216,40H40A16,16,0,0,0,24,56V200a16,16,0,0,0,16,16H216a16,16,0,0,0,16-16V56A16,16,0,0,0,216,40Zm-48,160H40V56H168V200Z"##,
                gradientID: "projectSidebarGlass",
                inverted: inverted
            )
        // icons.ts:20 (backIcon) — settings sidebar "Back" row
        case .back:
            return ##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" fill="#FFFFFF" viewBox="0 0 256 256"><path d="M222,128a6,6,0,0,1-6,6H54.49l61.75,61.76a6,6,0,1,1-8.48,8.48l-72-72a6,6,0,0,1,0-8.48l72-72a6,6,0,0,1,8.48,8.48L54.49,122H216A6,6,0,0,1,222,128Z"></path></svg>"##
        // Phosphor "bell" (fill) — titlebar activity glyph when nothing is
        // spinning but there are recently finished (unread) sessions.
        case .bell:
            return Self.glassSVG(
                path: ##"M221.8,175.94C216.25,166.38,208,139.33,208,104a80,80,0,1,0-160,0c0,35.34-8.26,62.38-13.81,71.94A16,16,0,0,0,48,200H88.81a40,40,0,0,0,78.38,0H208a16,16,0,0,0,13.8-24.06ZM128,216a24,24,0,0,1-22.62-16h45.24A24,24,0,0,1,128,216Z"##,
                gradientID: "bellGlass",
                inverted: inverted
            )
        // Phosphor "tree-structure" (fill) — marks a write-capable session in
        // the sidebar.
        case .orchestrator:
            return ##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" fill="#FFFFFF" viewBox="0 0 256 256"><path d="M144,96V80H128a8,8,0,0,0-8,8v80a8,8,0,0,0,8,8h16V160a16,16,0,0,1,16-16h48a16,16,0,0,1,16,16v48a16,16,0,0,1-16,16H160a16,16,0,0,1-16-16V192H128a24,24,0,0,1-24-24V136H72v8a16,16,0,0,1-16,16H24A16,16,0,0,1,8,144V112A16,16,0,0,1,24,96H56a16,16,0,0,1,16,16v8h32V88a24,24,0,0,1,24-24h16V48a16,16,0,0,1,16-16h48a16,16,0,0,1,16,16V96a16,16,0,0,1-16,16H160A16,16,0,0,1,144,96Z"></path></svg>"##
        // Phosphor "broadcast" (fill) — radiating signal: the sidebar footer
        // remote button (hosts and controllers).
        case .broadcast:
            return Self.glassSVG(
                path: ##"M168,128a40,40,0,1,1-40-40A40,40,0,0,1,168,128Zm40,0a79.74,79.74,0,0,0-20.37-53.33,8,8,0,1,0-11.92,10.67,64,64,0,0,1,0,85.33,8,8,0,0,0,11.92,10.67A79.79,79.79,0,0,0,208,128ZM80.29,85.34A8,8,0,1,0,68.37,74.67a79.94,79.94,0,0,0,0,106.67,8,8,0,0,0,11.92-10.67,63.95,63.95,0,0,1,0-85.33Zm158.28-4A119.48,119.48,0,0,0,213.71,44a8,8,0,1,0-11.42,11.2,103.9,103.9,0,0,1,0,145.56A8,8,0,1,0,213.71,212,120.12,120.12,0,0,0,238.57,81.29ZM32.17,168.48A103.9,103.9,0,0,1,53.71,55.22,8,8,0,1,0,42.29,44a119.87,119.87,0,0,0,0,168,8,8,0,1,0,11.42-11.2A103.61,103.61,0,0,1,32.17,168.48Z"##,
                gradientID: "broadcastGlass",
                inverted: inverted
            )
        // Phosphor square-split-vertical (fill) — merged-session agent mark.
        case .splitPanes:
            return ##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" fill="#FFFFFF" viewBox="0 0 256 256"><path d="M120,44V212a4,4,0,0,1-4,4H56a16,16,0,0,1-16-16V56A16,16,0,0,1,56,40h60A4,4,0,0,1,120,44Zm80-4H140a4,4,0,0,0-4,4V212a4,4,0,0,0,4,4h60a16,16,0,0,0,16-16V56A16,16,0,0,0,200,40Z"/></svg>"##
        // Phosphor square-split-vertical (line) — pane-header Split Pane.
        case .splitPane:
            return ##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" fill="none" viewBox="0 0 256 256"><rect width="256" height="256" fill="none"/><rect x="48" y="48" width="160" height="160" rx="8" fill="none" stroke="#FFFFFF" stroke-linecap="round" stroke-linejoin="round" stroke-width="16"/><line x1="128" y1="48" x2="128" y2="208" fill="none" stroke="#FFFFFF" stroke-linecap="round" stroke-linejoin="round" stroke-width="16"/></svg>"##
        // Same square with a horizontal divider — pane-header Split Pane Down.
        case .splitPaneDown:
            return ##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" fill="none" viewBox="0 0 256 256"><rect width="256" height="256" fill="none"/><rect x="48" y="48" width="160" height="160" rx="8" fill="none" stroke="#FFFFFF" stroke-linecap="round" stroke-linejoin="round" stroke-width="16"/><line x1="48" y1="128" x2="208" y2="128" fill="none" stroke="#FFFFFF" stroke-linecap="round" stroke-linejoin="round" stroke-width="16"/></svg>"##
        // Phosphor "globe" (line) — local-site link mark.
        case .globe:
            return ##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" fill="none" viewBox="0 0 256 256"><rect width="256" height="256" fill="none"/><circle cx="128" cy="128" r="96" fill="none" stroke="#FFFFFF" stroke-linecap="round" stroke-linejoin="round" stroke-width="16"/><path d="M168,128c0,64-40,96-40,96s-40-32-40-96,40-96,40-96S168,64,168,128Z" fill="none" stroke="#FFFFFF" stroke-linecap="round" stroke-linejoin="round" stroke-width="16"/><line x1="37.46" y1="96" x2="218.54" y2="96" fill="none" stroke="#FFFFFF" stroke-linecap="round" stroke-linejoin="round" stroke-width="16"/><line x1="37.46" y1="160" x2="218.54" y2="160" fill="none" stroke="#FFFFFF" stroke-linecap="round" stroke-linejoin="round" stroke-width="16"/></svg>"##
        case .settingsAppearance:
            return Self.glassSVG(
                path: ##"M128,24A104,104,0,1,0,232,128,104.11,104.11,0,0,0,128,24ZM40,128A88,88,0,0,1,190.2,65.8L65.8,190.2A87.76,87.76,0,0,1,40,128Z"##,
                gradientID: "settingsAppearanceGlass",
                inverted: inverted
            )
        case .settingsPresets:
            return Self.glassSVG(
                path: ##"M248,80v96a16,16,0,0,1-16,16H140a4,4,0,0,1-4-4V68a4,4,0,0,1,4-4h92A16,16,0,0,1,248,80ZM120,48V208a8,8,0,0,1-16,0V192H24A16,16,0,0,1,8,176V80A16,16,0,0,1,24,64h80V48a8,8,0,0,1,16,0ZM88,112a8,8,0,0,0-8-8H48a8,8,0,0,0,0,16h8v24a8,8,0,0,0,16,0V120h8A8,8,0,0,0,88,112Z"##,
                gradientID: "settingsPresetsGlass",
                inverted: inverted
            )
        case .settingsWorkspaces:
            return Self.glassSVG(
                path: ##"M216,40H40A16,16,0,0,0,24,56V200a16,16,0,0,0,16,16H216a16,16,0,0,0,16-16V56A16,16,0,0,0,216,40ZM64,152H48a8,8,0,0,1,0-16H64a8,8,0,0,1,0,16Zm0-32H48a8,8,0,0,1,0-16H64a8,8,0,0,1,0,16Zm0-32H48a8,8,0,0,1,0-16H64a8,8,0,0,1,0,16ZM216,200H88V56H216V200Z"##,
                gradientID: "settingsWorkspacesGlass",
                inverted: inverted
            )
        case .settingsRemote:
            return Self.glassSVG(
                path: ##"M120,76V188a4,4,0,0,1-4,4H96v16h15.73a8.18,8.18,0,0,1,8.25,7.47,8,8,0,0,1-8,8.53H64.27A8.18,8.18,0,0,1,56,216.53,8,8,0,0,1,64,208H80V192H32A24,24,0,0,1,8,168V96A24,24,0,0,1,32,72h84A4,4,0,0,1,120,76ZM248,48V208a16,16,0,0,1-16,16H152a16,16,0,0,1-16-16V48a16,16,0,0,1,16-16h80A16,16,0,0,1,248,48ZM203.9,181.57a12,12,0,1,0-10.34,10.33A12,12,0,0,0,203.9,181.57ZM224,103.47A8.18,8.18,0,0,0,215.73,96H168.27a8.18,8.18,0,0,0-8.25,7.47,8,8,0,0,0,8,8.53h48A8,8,0,0,0,224,103.47Zm0-32A8.18,8.18,0,0,0,215.73,64H168.27A8.18,8.18,0,0,0,160,71.47,8,8,0,0,0,168,80h48A8,8,0,0,0,224,71.47Z"##,
                gradientID: "settingsRemoteGlass",
                inverted: inverted
            )
        case .settingsTranscripts:
            return Self.glassSVG(
                path: ##"M216,40H40A16,16,0,0,0,24,56V200a16,16,0,0,0,16,16H216a16,16,0,0,0,16-16V56A16,16,0,0,0,216,40ZM72,144a8,8,0,0,1-4.89,7.37A7.86,7.86,0,0,1,64,152H52a8,8,0,0,1,0-16h4V88H52a8,8,0,0,1,0-16H64a8,8,0,0,1,6.91,4L92,112.12,113.09,76A8,8,0,0,1,120,72h12a8,8,0,0,1,0,16h-4v48h4a8,8,0,0,1,0,16H120a7.86,7.86,0,0,1-3.11-.63A8,8,0,0,1,112,144V109.59L98.91,132a8,8,0,0,1-13.82,0L72,109.59Zm128,40H88a8,8,0,0,1,0-16H200a8,8,0,0,1,0,16Zm0-32H160a8,8,0,0,1,0-16h40a8,8,0,0,1,0,16Zm0-32H160a8,8,0,0,1,0-16h40a8,8,0,0,1,0,16Z"##,
                gradientID: "settingsTranscriptsGlass",
                inverted: inverted
            )
        case .settingsNotifications:
            return Self.glassSVG(
                path: ##"M168,224a8,8,0,0,1-8,8H96a8,8,0,1,1,0-16h64A8,8,0,0,1,168,224Zm53.81-48.06C216.25,166.38,208,139.33,208,104a80,80,0,1,0-160,0c0,35.34-8.26,62.38-13.81,71.94A16,16,0,0,0,48,200H208a16,16,0,0,0,13.8-24.06Z"##,
                gradientID: "settingsNotificationsGlass",
                inverted: inverted
            )
        case .settingsExperimental:
            return Self.glassSVG(
                path: ##"M221.69,199.77,160,96.92V40h8a8,8,0,0,0,0-16H88a8,8,0,0,0,0,16h8V96.92L34.31,199.77A16,16,0,0,0,48,224H208a16,16,0,0,0,13.72-24.23Zm-90.08-42.91c-15.91-8.05-31.05-12.32-45.22-12.81l24.47-40.8A7.93,7.93,0,0,0,112,99.14V40h32V99.14a7.93,7.93,0,0,0,1.14,4.11L183.36,167C171.4,169.34,154.29,168.34,131.61,156.86Z"##,
                gradientID: "settingsExperimentalGlass",
                inverted: inverted
            )
        case .settingsSessions:
            return Self.glassSVG(
                path: ##"M232,96a16,16,0,0,0-16-16H184V48a16,16,0,0,0-16-16H40A16,16,0,0,0,24,48V176a8,8,0,0,0,13,6.22L72,154V184a16,16,0,0,0,16,16h93.59L219,230.22a8,8,0,0,0,5,1.78,8,8,0,0,0,8-8Zm-42.55,89.78a8,8,0,0,0-5-1.78H88V152h80a16,16,0,0,0,16-16V96h32V207.25Z"##,
                gradientID: "settingsSessionsGlass",
                inverted: inverted
            )
        case .settingsComputer:
            return Self.glassSVG(
                path: ##"M220.49,190.83a12,12,0,0,1,0,17L207.8,220.49a12,12,0,0,1-17,0l-56.56-56.57L115,214.09c0,.1-.08.21-.13.32a15.83,15.83,0,0,1-14.6,9.59l-.79,0a15.83,15.83,0,0,1-14.41-11L32.8,52.92A16,16,0,0,1,52.92,32.8L213,85.07a16,16,0,0,1,1.41,29.8l-.32.13-50.17,19.27ZM96,32a8,8,0,0,0,8-8V16a8,8,0,0,0-16,0v8A8,8,0,0,0,96,32ZM16,104h8a8,8,0,0,0,0-16H16a8,8,0,0,0,0,16ZM124.42,39.16a8,8,0,0,0,10.74-3.58l8-16a8,8,0,0,0-14.31-7.16l-8,16A8,8,0,0,0,124.42,39.16Zm-96,81.69-16,8a8,8,0,0,0,7.16,14.31l16-8a8,8,0,1,0-7.16-14.31Z"##,
                gradientID: "settingsComputerGlass",
                inverted: inverted
            )
        case .settingsBrowser:
            return Self.glassSVG(
                path: ##"M128,24h0A104,104,0,1,0,232,128,104.12,104.12,0,0,0,128,24Zm78.36,64H170.71a135.28,135.28,0,0,0-22.3-45.6A88.29,88.29,0,0,1,206.37,88ZM216,128a87.61,87.61,0,0,1-3.33,24H174.16a157.44,157.44,0,0,0,0-48h38.51A87.61,87.61,0,0,1,216,128ZM128,43a115.27,115.27,0,0,1,26,45H102A115.11,115.11,0,0,1,128,43ZM102,168H154a115.11,115.11,0,0,1-26,45A115.27,115.27,0,0,1,102,168Zm-3.9-16a140.84,140.84,0,0,1,0-48h59.88a140.84,140.84,0,0,1,0,48Zm50.35,61.6a135.28,135.28,0,0,0,22.3-45.6h35.66A88.29,88.29,0,0,1,148.41,213.6Z"##,
                gradientID: "settingsBrowserGlass",
                inverted: inverted
            )
        case .settingsWorktrees:
            return Self.glassSVG(
                path: ##"M232,76a28,28,0,1,0-31.47,27.71A68.1,68.1,0,0,1,136,167.48V192a8,8,0,0,1-16,0V167.48A68.1,68.1,0,0,1,55.47,103.71,28,28,0,1,0,24,76a28,28,0,0,0,31.47,27.71,52.08,52.08,0,0,0,48.53,48.15V80a8,8,0,0,1,16,0v71.86a52.08,52.08,0,0,0,48.53-48.15A28,28,0,0,0,232,76ZM56,64A12,12,0,1,1,44,76,12,12,0,0,1,56,64ZM88,204a12,12,0,1,1-12-12A12,12,0,0,1,88,204ZM200,88a12,12,0,1,1,12-12A12,12,0,0,1,200,88Z"##,
                gradientID: "settingsWorktreesGlass",
                inverted: inverted
            )
        case .settingsAdvanced:
            return Self.glassSVG(
                path: ##"M216,40H40A16,16,0,0,0,24,56V200a16,16,0,0,0,16,16H216a16,16,0,0,0,16-16V56A16,16,0,0,0,216,40ZM101.66,141.66l-32,32a8,8,0,0,1-11.32-11.32L84.69,136,58.34,109.66a8,8,0,0,1,11.32-11.32l32,32A8,8,0,0,1,101.66,141.66ZM192,176H120a8,8,0,0,1,0-16h72a8,8,0,0,1,0,16Z"##,
                gradientID: "settingsAdvancedGlass",
                inverted: inverted
            )
        }
    }

    private static func glassSVG(path: String, gradientID: String, inverted: Bool) -> String {
        """
        <svg xmlns="http://www.w3.org/2000/svg" width="32" height="32" fill="none" viewBox="0 0 256 256"><defs><linearGradient id="\(gradientID)" x1="34" y1="40" x2="224" y2="218" gradientUnits="userSpaceOnUse">\(glassStops(inverted: inverted))</linearGradient></defs><path d="\(path)" fill="url(#\(gradientID))"></path><path d="\(path)" fill="none" stroke="#FFFFFF" stroke-opacity="0.30" stroke-width="8" stroke-linejoin="round"></path></svg>
        """
    }

    /// The glass-fill gradient stops. Default ramps bright→faint along the
    /// top-left→bottom-right diagonal (reads well on dark chrome). `inverted`
    /// reverses the opacity ramp so the faint end leads — used in light mode so
    /// the icon's tint doesn't pool solid in the top-left corner.
    private static func glassStops(inverted: Bool) -> String {
        if inverted {
            return ##"<stop offset="0" stop-color="#FFFFFF" stop-opacity="0.52"/><stop offset="0.55" stop-color="#FFFFFF" stop-opacity="0.80"/><stop offset="1" stop-color="#FFFFFF" stop-opacity="0.98"/>"##
        }
        return ##"<stop offset="0" stop-color="#FFFFFF" stop-opacity="0.98"/><stop offset="0.45" stop-color="#FFFFFF" stop-opacity="0.80"/><stop offset="1" stop-color="#FFFFFF" stop-opacity="0.52"/>"##
    }

    /// The Svelte branch icon string carries `style="transform: rotate(90deg)"`.
    fileprivate var rotationDegrees: Double { self == .branch ? 90 : 0 }
}

@MainActor
enum ChromeIconStore {
    private struct Key: Hashable {
        let icon: ChromeIcon
        let inverted: Bool
    }

    private static var cache: [Key: NSImage?] = [:]

    static func image(for icon: ChromeIcon, inverted: Bool = false) -> NSImage? {
        let key = Key(icon: icon, inverted: inverted)
        if let cached = cache[key] { return cached }
        let image = NSImage(data: Data(icon.svg(inverted: inverted).utf8))
        image?.isTemplate = true
        cache[key] = image
        return image
    }
}

/// Renders one Svelte chrome icon at the pixel size the web app uses,
/// tinted by the surrounding foregroundStyle. Falls back to a near-enough
/// SF Symbol if SVG decoding ever fails.
struct ChromeIconView: View {
    let icon: ChromeIcon
    var size: CGFloat = 16

    @Environment(\.colorScheme) private var colorScheme

    var body: some View {
        if let image = ChromeIconStore.image(for: icon, inverted: colorScheme == .light) {
            Image(nsImage: image)
                .resizable()
                .interpolation(.high)
                .aspectRatio(contentMode: .fit)
                .rotationEffect(.degrees(icon.rotationDegrees))
                .frame(width: size, height: size)
        } else {
            Image(systemName: fallbackSymbol)
                .font(.system(size: size * 0.85, weight: .medium))
                .frame(width: size, height: size)
        }
    }

    private var fallbackSymbol: String {
        switch icon {
        case .folderClosed, .folderOpen, .folderSimple: return "folder"
        case .branch, .gitBranch: return "arrow.triangle.branch"
        case .pin: return "pin"
        case .pinSlash: return "pin.slash"
        case .paneUnpin: return "rectangle.portrait.and.arrow.right"
        case .pushPin: return "pin"
        case .settings: return "gearshape"
        case .addProjectPlus, .plus, .plusBold: return "plus"
        case .collapseAll: return "chevron.up.chevron.down"
        case .mobile: return "iphone"
        case .sidebarToggle: return "sidebar.left"
        case .projectSidebar: return "sidebar.right"
        case .back: return "arrow.left"
        case .bell: return "bell"
        case .orchestrator: return "point.3.connected.trianglepath.dotted"
        case .broadcast: return "dot.radiowaves.left.and.right"
        case .splitPanes, .splitPane: return "rectangle.split.2x1"
        case .splitPaneDown: return "rectangle.split.1x2"
        case .globe: return "globe"
        case .settingsAppearance: return "circle.righthalf.filled"
        case .settingsPresets: return "character.textbox"
        case .settingsWorkspaces: return "sidebar.left"
        case .settingsRemote: return "iphone"
        case .settingsTranscripts: return "doc.text"
        case .settingsNotifications: return "bell"
        case .settingsExperimental: return "testtube.2"
        case .settingsSessions: return "bubble.left.and.bubble.right"
        case .settingsComputer: return "cursorarrow.click"
        case .settingsBrowser: return "globe"
        case .settingsWorktrees: return "arrow.triangle.branch"
        case .settingsAdvanced: return "terminal"
        }
    }
}
