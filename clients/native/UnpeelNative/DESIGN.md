# Unpeel Design Spec (extracted from apps/desktop frontend)

Extracted 2026-06-11 from `apps/desktop/src` + `packages/glass-ui`. Dark theme is primary;
light values given where they differ. All CSS `color-mix(in srgb, ...)` values below are
pre-resolved to concrete hex/rgba.

Token source of truth: `packages/glass-ui/src/lib/styles/glass.css` (`:root` 1–79,
`[data-theme="dark"]` 125–184, `[data-theme="light"]` 186–245). The whole shell is tinted by
`--glass-shell-tint-source`, which is set at runtime to the terminal background of the active
color scheme (`stores/theme.ts:35-40` → `terminal/theme.ts:179-184`). For the **default**
scheme that is `#222228` dark / `#ffffff` light — all resolved colors below assume default.
Other schemes (purple `#322e40`, blue `#2a3240`, moss `#2c3630`, clay `#3a302a`) re-tint
everything through the same mix formulas; keep colors derived, not hardcoded, if scheme
support is wanted.

---

## 1. Window chrome

- Tauri window (`src-tauri/tauri.conf.json` → `app.windows[0]`):
  - `titleBarStyle: "Overlay"`, `hiddenTitle: true`, `transparent: true`
  - default 1200×800, min 800×600, resizable
  - traffic lights inset: `x: 12, y: 17` (button centers land at y≈15pt; see comment in
    `App.svelte:1760-1763`)
  - window title string is empty (`App.svelte:30`)
- Native vibrancy (`src-tauri/src/macos_sidebar_blur.rs:112-144`):
  - window `setOpaque(false)`, clear background color
  - full-content `NSVisualEffectView` material **`.underWindowBackground`**, behindWindow,
    followsWindowActiveState
  - sidebar-width `NSVisualEffectView` material **`.sidebar`** layered on the left
    (width synced to the live sidebar width)
  - AppKit equivalent: keep exactly this — it is already the native construct.
- Webview tint layers painted over the vibrancy (dark):
  - sidebar (`--glass-chrome-bg`, glass.css:156-166): two fills — `#222228` @ 18% alpha over
    `#2B2E37` @ 16% alpha
  - main content (`--glass-content-bg`, glass.css:167-177): `#222228` @ 12% over `#2B2E37` @ 20%
  - light: sidebar = white @ 10% + white @ 26%; main = white @ 8% + white @ 32%
  - opaque fallback when native chrome is unavailable: `#2B2E37` (`--background`,
    `styles/global.css:40-65`)
- Titlebar (custom, drawn in webview; `App.svelte:1747-1857`):
  - height **38px** (`--titlebar-height`, glass.css:2); whole strip is a drag region,
    double-click toggles maximize (`App.svelte:32-41`)
  - title text centered absolutely: **13px / weight 600**, color = muted text
    rgba(243,245,251,0.66)
  - format: `projectname` or `parent / worktree[ / branch]` — separators are literal `/`
    spans at **0.55 opacity**, workspace part weight 500 (`App.svelte:1452-1476, 1843-1857`)
  - optional 14×14 tool icon left of the title, brand-colored: claude `#d97757`,
    codex `#10a37f`, amp `#f97316`, gemini `#6b86d8`, pi `#7c95ff`,
    terminal ≈ `#d6d9e1` (fg 78% + muted 22%) (`App.svelte:1868-1890`)
  - titlebar background = the terminal background color (`#222228` dark default) while a
    terminal is displayed, otherwise transparent (`App.svelte:290-294`)
  - sidebar-toggle button: 28×28, radius 6, fixed at left 72px / top 1px (next to traffic
    lights); in fullscreen moves to left 8px / top 5px. "+ new session" sibling at left 104px.
    Icon 16px, muted color, hover = fg-10% bg. (`App.svelte:1757-1818`; note hover uses
    `var(--bg-hover)` which is **undefined** in CSS — effectively no hover bg today; native
    can use rgba(243,245,251,0.10))
- No window corner-radius is set by the app; macOS default window radius applies.

## 2. Color palette (dark, default scheme — resolved)

| Role | Value | Source |
| --- | --- | --- |
| App background (opaque fallback) | `#2B2E37` | `--background`, glass.css:128 |
| Sidebar bg | vibrancy `.sidebar` + `#222228`@18% + `#2B2E37`@16% | glass.css:156, Sidebar.svelte:652-655 |
| Main/terminal pane bg | vibrancy `.underWindowBackground` + `#222228`@12% + `#2B2E37`@20% | glass.css:167, App.svelte:1737-1745 |
| Terminal surface (opaque) | `#222228` | terminal/theme.ts:10 |
| Primary text | `#F3F5FB` | `--foreground`, glass.css:129 |
| Muted/secondary text | `rgba(243,245,251,0.66)` | `--muted-foreground`, glass.css:137 |
| Card | `#30333C` | glass.css:130 |
| Secondary (scrollbar thumb) | `#4A4F5C` | glass.css:134 |
| Muted surface | `#3D424B` | glass.css:136 |
| Accent surface | `#555C6F` | glass.css:139 |
| Border (full-strength) | `#808697` | glass.css:140 |
| Hairline/glass border | `rgba(255,255,255,0.22)` | `--glass-border-color`, glass.css:178 |
| Hover row bg (project+session) | `rgba(243,245,251,0.10)` (fg 10%) | glass.css:182-183 |
| Active/selected row bg | `rgba(255,255,255,0.16)` | `--glass-active-tint`, glass.css:148 |
| Active row strong | `rgba(255,255,255,0.20)` | glass.css:149 |
| Control bg (buttons) | fg 10% = `rgba(243,245,251,0.10)`; hover 14% | glass.css:143-144 |
| Busy spinner (generic) | fg/muted mix ≈ `#B9BDC9`; per-tool brand colors (§5) | GlassSpinner.svelte:41 |
| Attention | `#f59e0b` (session/worktree dot), `#eab308` (project dot) | ProjectItem.svelte:2484, 1923 |
| Unread badge | `#60a5fa` | ProjectItem.svelte:1928, 2637 |
| Danger/error | `#ef4444` | `--danger`, glass.css:23 |
| Focus ring | `#a7b1c3` | glass.css:142 |
| Text selection | fg 18% = `rgba(243,245,251,0.18)` | glass.css:121-123 |

Light theme (default scheme, tint = white): background/card `#FFFFFF`, foreground `#111217`,
muted text `rgba(17,18,23,0.60)`, secondary `#F8F9FA`, muted surface `#F6F7F9`, accent
`#F4F5F8`, border `#CED3DC`, glass border `rgba(0,0,0,0.16)`, active row bg solid `#FFFFFF`,
ring `#2d313a` (glass.css:186-245).

## 3. Typography

UI font (`--font-sans`, glass.css:44-52): **system font** — `"SF Pro Text", "SF Pro Display",
ui-sans-serif, system-ui, -apple-system, ...`. In Swift use `NSFont.systemFont`.
Body base: 14px / line-height 1.5, antialiased (glass.css:85-94).
Mono (`--font-mono`, glass.css:53-62): `"SF Mono", ui-monospace, "Cascadia Code",
"JetBrains Mono", Menlo, ...` — used for branch names, worktree pickers.

| Element | Size / weight | Source |
| --- | --- | --- |
| Sidebar project name | 12px / 600, 0.6 opacity (full opacity + shimmer when busy) | ProjectItem.svelte:2053-2064 |
| Session row title | 12px / 600, line-height 1.2 | ProjectItem.svelte:2605-2613 |
| Session age | 9px, 0.7 opacity, right-aligned min-width 24px | ProjectItem.svelte:2653-2662 |
| Branch chip text | mono 10px / 500 (line-height 15px in chip) | ProjectItem.svelte:2522-2536 |
| Titlebar title | 13px / 600 (workspace segment 500) | App.svelte:1820-1853 |
| Section header ("Worktrees" link, slide-in header) | 12-13px / 600 | ProjectItem.svelte:2119-2124, Sidebar.svelte:712-720 |
| Buttons (GlassButton) | 12px / 600 (xs: 11px) | GlassButton.svelte |
| Launcher title | 14px / 600; path hint 11px rgba(255,255,255,0.35) | SessionLauncherView.svelte:328-342 |
| Dropdown item | 12px / 500; group label 10px / 600 uppercase, ls 0.04em | GlassDropdown.svelte |
| Tag chip (sm) | 10px / 650, ls 0.02em | GlassTag.svelte |

### Terminal (xterm config — `TerminalView.svelte:107-111`, `xtermDriver.ts:73-89`)

- fontFamily: `'JetBrains Mono', 'SF Mono', ui-monospace, monospace` (JetBrains Mono is
  bundled-first; SF Mono is the system fallback)
- fontSize **13**, lineHeight **1**, letterSpacing **0**
- cursorBlink true, cursorStyle `block`, cursorInactiveStyle `outline`, scrollback **5000**
- allowTransparency true (theme background is set to opaque `#222228` though;
  `getOpaqueTerminalTheme`, terminal/theme.ts:149-166)

Full xterm dark theme (terminal/theme.ts:9-31, default scheme overrides 64-70):

```
background  #222228   foreground #fafafa
cursor      #fafafa (foreground; hidden variant = background)
cursorAccent #222228  selectionBackground #3a3a40
black   #1c1c22   brightBlack   #6e6e76   (base table says #52525b; default-scheme override wins)
red     #ef4444   brightRed     #f87171
green   #22c55e   brightGreen   #4ade80
yellow  #eab308   brightYellow  #facc15
blue    #3b82f6   brightBlue    #60a5fa
magenta #a855f7   brightMagenta #c084fc
cyan    #06b6d4   brightCyan    #22d3ee
white   #a1a1aa   brightWhite   #fafafa
```

Light: background `#ffffff`, foreground `#09090b`, selection `#d4d4d8`, ANSI = the 600-weight
Tailwind hues (theme.ts:32-54).

## 4. Sidebar anatomy

- Width: **resizable**, default **300px**, min **220**, max **520** (Sidebar.svelte:142,159-160),
  persisted (`localStorage unpeel.sidebar.width`). Resizer: 8px-wide invisible hit area
  straddling the right edge with a centered 1px line of glass-border at 0.25 opacity
  (≈ rgba(255,255,255,0.055)) (Sidebar.svelte:657-678). Sidebar collapse is instant
  (component unmount), width transition 0.15s when not dragging.
- Top: transparent drag region of titlebar height (38px) (Sidebar.svelte:639-646).
- Project list (Sidebar.svelte:752-784): scrollable, padding `40px 8px 12px`
  (titlebar+2 top), column gap 2px, **scrollbar hidden**, content masked with a vertical
  fade: transparent → opaque at 38px from top, fades out over last 26px.
  Native equivalent: NSScrollView with hidden scroller + gradient mask layer.
- Project row (ProjectItem.svelte:1781-1832):
  - min-height **28px**, padding `2px 7px` + indent `depth*14px` left, radius **9px**, gap 7px
  - hover: rgba(243,245,251,0.10); active project row itself is transparent (only highlighted
    `--glass-active-tint` while the launcher targets it); transitions 0.12s ease
  - leading: 18×18 folder icon (16px glyph), swaps to a drag-handle glyph on hover
    (expand-toggle keeps folder); when collapsed shows a 6px state dot top-right of the icon:
    attention `#eab308` w/ 4px 20% halo, unread `#60a5fa` (ProjectItem.svelte:1912-1929)
  - name: 12px/600 @ 0.6 opacity; busy = shimmer (left-to-right gradient sweep, 1.8s linear
    infinite) (ProjectItem.svelte:2066-2085)
  - trailing actions appear on hover (opacity 0→1, 0.12s): "…" menu + "+" new session,
    22×22 buttons radius 8, muted → fg on hover. Quick-preset strip lives between them:
    a 24px-high pill (radius 8, bg = active-tint @ 76%) that expands on hover to
    `n*24+23px` wide revealing per-tool icon buttons (14px icons, 0.72 opacity);
    expansion `inline-size 0.28s cubic-bezier(0.22,1,0.36,1)` (ProjectItem.svelte:1947-2051)
- Session list (ProjectItem.svelte:2225-2253): column, gap **1.5px**, indented by project
  indent. Accordion: height 340ms `cubic-bezier(0.16,1,0.3,1)`, content fade 220ms +
  translateY(-6px)/scale(0.992); per-row stagger 14ms, row-enter 380ms
  `cubic-bezier(0.18,0.86,0.26,1)` from `translate(-5px,-4px) scale(0.988)`.
- Session row (ProjectItem.svelte:2281-2331):
  - min-height **28px**, padding `2px 9px`, radius **9px**, gap 7px
  - default text color: live = foreground, dead = muted @ 0.82 row opacity, hover = fg +
    rgba(243,245,251,0.10) bg, active = rgba(255,255,255,0.16) bg
  - leading 16px slot with a 13×13 icon stack: busy = spinner (overlay-centered),
    attention = 6px dot `#f59e0b` + 4px halo @ 20%; hovering the row swaps the
    indicator for a pin button (22×22, radius 6)
  - title truncates with ellipsis; optional branch chip (mono 10px, radius 7, padding 0 6px,
    bg fg-10%); unread = 7px `#60a5fa` dot inline after the title
  - trailing meta: relative age ("now/5m/3h/2d", 9px @ 0.7); on hover the age is replaced by
    tag + archive icon buttons (22×22 radius 6, 13px icons); archive click arms a "Confirm"
    state: auto-width pill, bg `#ef4444` @ 15% (25% hover), text `#ef4444`, 11px/500
    (ProjectItem.svelte:2682-2746)
  - holding ⌘ shows `⌘1…⌘9` shortcut hints in place of the age (9px/500 @ 0.7)
  - tag chip: GlassTag sm — 10px/650, radius 6, bg = tagColor 72% + white 28%, text =
    `--background`
- Archived sessions never render as a sidebar subsection. Right-click the
  owning project and choose `Archived (N)` to open the project-scoped
  library in the main pane. Archiving stops the hosted terminal but keeps the
  session directory; the library offers Resume, Restore to sidebar, transcript
  copy, and permanent removal.
- "Worktrees" link row (when worktrees enabled; ProjectItem.svelte:1362-1383, 2087-2141):
  same session-row geometry; branch-split icon @ 0.8 opacity (spinner when any worktree
  busy; 6px `#f59e0b` attention dot top-right), label "Worktrees" 12px/600, count badge
  (min-width 16px, 10px text, line-height 16px, radius 8, bg fg-10%), `›` chevron @ 0.6.
- Worktrees slide-in view (Sidebar.svelte:464-487): list panel slides
  (`fly x: ±140, 200ms`) — outgoing tree exits left, worktree panel enters from the right;
  footer stays. Header: `‹` back button (radius 7, 17px glyph) + project name 13px/600.
  Footer of the list: "+ New worktree" ghost row (12px/500, radius 9, muted → fg hover).
- Skeleton rows while loading: pill-shaped shimmer lines, 1.25s ease-in-out pulse
  (ProjectItem.svelte:2408-2449).
- Sidebar footer (Sidebar.svelte:786-814): single row, padding `0 7.5px 7.5px`,
  settings ⚙ + add ＋ on the left (xs ghost buttons 26×26, radius 7, 14px icons, muted),
  collapse-all on the right. No section headers anywhere — the sidebar is just the project
  tree + footer.
- Global scrollbars elsewhere (styles/global.css:81-97): 8px wide, transparent track, thumb
  `#4A4F5C` radius 4, hover `rgba(243,245,251,0.66)`.

## 5. Session status visuals

States: `busy | idle | attention | exited` (+ unread overlay, + restarting).

- **Busy**: GlassSpinner, not a pulsing dot. Default variant is a per-tool **braille glyph
  spinner**: frames `⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏` at **120ms/frame** (spinner.ts:7-8). Size `sm` = 11px box,
  glyph font = mono 700 at 1.34× box (≈14.7px), subtle 6px currentColor glow.
  Colors (GlassSpinner.svelte:65-83): claude `#d97757`, codex `#39d353`, pi `#7c95ff`,
  opencode `#8f8787`, gemini cycles per frame `#6ea8ff → #7f8cff → #a67eff → #d16d95 → …`
  (spinner.ts:10-21), fallback/unknown = "ring": a 2px conic-gradient arc ring rotating
  **0.85s linear infinite**, colors mixed from fg/muted (≈`#B9BDC9` fading tail).
  Busy project name additionally shimmers (§4).
- **Idle**: no indicator at all (icon slot empty until hover reveals the pin).
- **Attention**: static 6px dot `#f59e0b` with `0 0 0 4px rgba(245,158,11,0.20)` halo,
  centered in the icon stack (ProjectItem.svelte:2475-2494). Collapsed-project variant uses
  `#eab308`. No animation.
- **Exited/dead**: row drops to muted color at 0.82 opacity; main pane shows the dead-session
  screen — 32px tool icon (brighter brand tints: claude `#f57c52`, codex `#4cc38a`,
  amp `#fb923c`, gemini `#7aa1f4`, pi `#65a8ff`, terminal `#cfd7e5`), label 14px/500,
  "Restart" glass button (App.svelte:1518-1529, 1995-2026).
- **Unread**: 7px `#60a5fa` dot after the session title; 6px on collapsed project icon.
- **Restarting**: spinner in icon slot, meta shows the word "restarting", row non-interactive.

## 6. Terminal area

- The terminal fills the main panel below the 38px titlebar; container is absolutely
  positioned, no chrome/toolbar of its own (App.svelte:1478-1496).
- Viewport padding: **8px top, 0 right, 8px bottom, 16px left**
  (TerminalView.svelte:1383). Exception: OpenCode sessions get zero padding and the
  container paints the opaque terminal surface itself.
- Background: xterm paints opaque `#222228` (default dark); the surrounding pane shows the
  glass-content tint. Active-session switch crossfade: workspace screen opacity 0.14s ease.
- Terminal scrollbar: xterm's overlay scrollbar restyled to a pill — slider inset `3px 4px`,
  radius 999, fg @ 22% → 32% hover → 40% active (TerminalView.svelte style block).
- Scroll-to-bottom: circular 36px glass button at bottom 16 / right 20; hidden state
  translateY(8px) + opacity 0, shows at 0.88 opacity, 0.2s ease.
- Loading state: small spinner + "Loading terminal…" 13px muted, centered.
- Empty state ("No sessions yet."): centered column, 8px gap, 14px muted text + glass
  buttons ("Restart: <label>" filled, "+ New Session" ghost).
- Launcher view (SessionLauncherView.svelte:308-533): replaces the terminal area
  (slide-less, just shown/hidden), padded `24px 28px 0`, centered column
  `min(520px, vw-72px)`, 18px gap:
  - title "New session in {project}" 14px/600 centered; path hint 11px
    rgba(255,255,255,0.35)
  - worktree picker pill: 24px min-height, padding 3px 12px, radius 12, 1px border of
    fg-10%; "Worktree" label 11px muted + branch value mono 11px + `▾` 9px;
    opens a GlassDropdown (§7)
  - preset rows: full-width, 28px min-height, padding 2px 12px, radius 9, gap 10,
    transparent → fg-10% hover; 16px tool icon (brand colors as §1) + command text 13px;
    first entry is always "terminal" (blank preset); trailing "Manage presets" row
  - launcher/onboarding screens sit on a near-flat gradient of the glass-content tint
    (App.svelte:1948-1961)
- New-worktree screen (NewWorkspaceView.svelte): same centered-column pattern; title
  14px/600; branch input `min(420px,100%)`, padding 8px 14px, radius 10, 1px fg-10% border
  → foreground border on focus, mono input text.

## 7. Micro-interactions / radii / motion

Radii in active use: rows 9px; small icon buttons 6–8px; GlassButton 10px (xs 7px);
dropdown 10px (items 7px); pills/badges 999px; tags 5/6/7px (xs/sm/md); worktree trigger
12px; update banner 12px. (`--radius-small/medium/large` = 10/14/18 exist but rows use
literal values above.)

Durations/easings worth copying:
- Row/button hover: background+color **0.12s ease** (everywhere in sidebar)
- GlassButton hover: 0.14s ease, lift `translateY(-1px)` (non-ghost only)
- Sidebar width change (non-drag): 0.15s
- Worktrees slide-in: fly x 140px, **200ms** (Svelte fly default cubicOut)
- Session accordion: height **340ms cubic-bezier(0.16,1,0.3,1)**, fade 180–220ms ease-out
- Session row entrance: 380ms `cubic-bezier(0.18,0.86,0.26,1)`, stagger **14ms** per row,
  from `translate(-5px,-4px) scale(0.988)`
- Quick-preset strip expand: **0.28s cubic-bezier(0.22,1,0.36,1)** (a snappy ease-out-quint)
- Spinner: braille 120ms/frame; ring 0.85s/turn linear
- Shimmer (busy project name): 1.8s linear infinite; skeleton pulse 1.25s
- Dropdown enter: 0.12s ease-out, translateY(-4px) scale(0.97) → identity
- Modal/backdrop: 0.15s ease-out fade (+ translateY(-8px) scale(0.98) for modal)
- Workspace screen swap: opacity 0.14s; launch splash: 0.45s in / 0.42s out
- Typewriter title reveal: 20–40ms/char with blinking 1px caret (0.6s steps)
  (ProjectItem.svelte:184-218, 2615-2629)
- `prefers-reduced-motion`: stagger/entrance animations disabled

GlassDropdown (context for pickers; GlassDropdown.svelte): radius 10, padding 4, bg =
`#2B2E37` @ 82% + faint SVG noise + blur(32px) saturate(150%), 1px border fg-10%,
double drop shadow (4/16 + 12/40 black @ 14%/10%). Native menus (project/session context
menus) already use real `NSMenu` via Tauri — keep native.

## 8. Won't translate to AppKit 1:1 — suggested equivalents

- `color-mix(...)` chains → precompute (values in this doc) or `NSColor.blended(withFraction:)`.
- `backdrop-filter: blur()` on buttons/dropdowns → either drop (buttons read fine as flat
  alpha fills) or small `NSVisualEffectView` (.hudWindow/.popover material) for dropdowns.
- CSS mask fade on the project list → `CAGradientLayer` mask on the scroll view.
- Conic-gradient ring spinner → `CAShapeLayer` arc with rotation animation, or just use the
  braille-glyph spinner (it's an attributed string — trivial in AppKit) for all variants.
- Text shimmer on busy project names → `CAGradientLayer` text mask animation, or skip and
  rely on the spinner (lowest-value effect).
- `interpolate-size` accordion → `NSAnimationContext`/constraint animation with the same
  curve (`CAMediaTimingFunction(controlPoints: 0.16, 1, 0.3, 1)`).
- xterm.js itself → libghostty; feed it the theme table from §3 and the same font stack
  (JetBrains Mono bundled, SF Mono fallback, 13pt, cell line-height 1.0).
- `-webkit-app-region: drag` titlebar → standard `NSWindow` with
  `.titlebarAppearsTransparent`, `.fullSizeContentView`, hidden title, traffic-light
  repositioning to (12, 17).
