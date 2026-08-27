# Claude card design QA

**Source visual truth**

- `/Users/tommyvedvik/Desktop/HPxcbhqaQAALsPO.jpg`
- 1440 × 1690 px, light appearance, OpenUsage expanded Claude share card.
- `/Users/tommyvedvik/.unpeel/dropped-images/drop-1787739788372-94CEA20D-7735-454F-AC73-671AF83E58FD.png`
- 1464 × 1442 px, dark appearance, pre-change Ratatui dashboard showing the
  card fills and drag-handle glyph targeted by the refinement.

**Rendered implementation**

- `/tmp/unpeel-usage-design-qa/claude-flat-light.png`
- 840 × 704 px, representing an 84 × 32 Ratatui viewport at 10 × 22 px per terminal cell.
- `/tmp/unpeel-usage-design-qa/scrollbar-fixed.png`
- 720 × 384 px, showing exact 72 × 8 Ratatui top and bottom scroll states at
  10 × 22 px per terminal cell, separated by 32 px for comparison.
- Native terminal UI; CSS size and browser device density do not apply. The capture uses a 1× rasterization of the Ratatui `TestBackend` cell grid.
- State: light palette, selected Claude card, `Team 5x`, reference usage fixture, details collapsed.

**Full-view comparison evidence**

- Provider/plan hierarchy matches: Claude followed by `Team 5x`.
- Quota order and structure match: Session, Weekly, Fable; used-percent fill; reset text; amber spare projection; red run-out projection; and even-pace markers.
- Lower-card hierarchy matches: Extra Usage, inline right-aligned Usage Trend, Today, Yesterday, Last 30 Days.
- The card inherits the terminal background with no separate fill. The
  dark/muted text hierarchy, blue/amber/red meter semantics, gray tracks, and
  focus border remain intact.
- Terminal-specific chrome (app header, focus border, keyboard footer, monospaced font) is intentional: the requested app remains 100% native Ratatui rather than imitating a bitmap/web card.

**Focused-region comparison evidence**

- The scrollbar pass uses
  `/tmp/unpeel-usage-design-qa/scrollbar-before-crop.png`, a 164 × 1400 px
  right-edge crop of the supplied dark app screenshot, alongside the exact
  post-fix top/bottom raster. The post-fix thumb visibly begins on the first
  track row and ends on the final track row; the bottom card border remains
  flush with the viewport instead of leaving empty rows.
- Source-only graphical marks (Claude logo and flame) were not approximated
  with emoji or custom drawings. The decorative drag handle was removed; the
  terminal-native selection bar and colored text retain the interaction and
  status cues.

**Required fidelity surfaces**

- Fonts and typography: expected platform adaptation. The source uses proportional UI type; the implementation uses the terminal's monospace face with bold label hierarchy and no wrapping.
- Spacing and layout rhythm: one-cell card padding on every side, consistent blank-row spacing between quota groups, compact lower metrics, and right-aligned values. No clipping at the 84 × 32 reference viewport.
- Colors and visual tokens: terminal-default card background plus semantic
  blue/amber/red meter states; adaptive, explicit light, and explicit dark
  palettes are separately render-tested.
- Image and asset fidelity: no raster imagery is part of the Ratatui card. Source icons were intentionally omitted instead of replaced with low-fidelity terminal approximations.
- Copy and content: all user-facing rows, used-percent language, reset language, pacing copy, plan, and history periods match. `est` remains on local dollar estimates to avoid presenting inferred spend as provider-billed fact.

**Findings**

- No actionable P0, P1, or P2 findings remain. The scrollbar correction also
  restores access to clipped content in short terminals.

**Comparison history**

1. Initial comparison found one P2: Usage Trend rendered as a full-width second line, while the source places the spark bars to the right of the label.
2. Fixed by making the Claude Usage Trend a one-row, right-aligned Ratatui `Sparkline` while retaining narrow-viewport bounds.
3. Post-fix comparison confirmed the quota and lower-card hierarchy now align without clipping or collisions.
4. The later dark-app comparison found the filled card rectangles visually
   heavier than requested and a non-functional drag handle in every provider
   header.
5. Removed both card background styles and the drag handle. The post-change
   raster confirms the terminal background now runs continuously through the
   cards while borders, padding, meters, hierarchy, and the selection cue stay
   legible.
6. A later P1 interaction finding showed the scrollbar was derived from
   provider-selection jumps rather than actual visible rows. Its thumb used
   total rows as Ratatui scroll positions, could not reach the track bottom,
   clipped tall cards, and mouse-wheel input changed selection instead of
   scrolling.
7. Fixed by rendering the provider list on a virtual row canvas, clipping a
   continuous viewport, sizing the thumb from viewport/total rows, and storing
   a real row offset. Wheel, PageUp/PageDown, track click/drag, selection
   reveal, and terminal resize now update the same scroll state. The exact
   top/bottom raster and backend assertions confirm both endpoints and the
   proportional thumb.

**Interaction and regression evidence**

- Provider selection, detail toggle, mouse hit regions, scrollbar behavior,
  narrow widths, tiny terminals, card padding, meter colors, inherited card
  backgrounds, absent drag handles, and adaptive/light/dark palettes are
  covered by Ratatui backend tests.
- Scrollbar QA specifically covers proportional thumb length, exact first and
  last track positions, continuous partial-card clipping, and a bottom viewport
  with no blank tail.
- Browser interactions and console checks are not applicable to this native terminal binary.

**Open questions**

- None.

**Implementation checklist**

- [x] Real Claude limit data mapped into the visual hierarchy.
- [x] OpenUsage-style pacing annotations and markers.
- [x] Calendar-day local history and inline trend.
- [x] Light/dark/adaptive color coverage.
- [x] Same-input visual comparison after the P2 fix.
- [x] Flat inherited card background and drag-handle removal.
- [x] Continuous row scrolling and proportional, interactive scrollbar.

**Follow-up polish**

- None required for this Ratatui adaptation.

final result: passed
