export const UI_PROTOCOL_NAME = "unpeel.ui" as const;
export const UI_PROTOCOL_MIN_VERSION = 1 as const;
export const UI_PROTOCOL_MAX_VERSION = 1 as const;
export const UI_PROTOCOL_VERSION = UI_PROTOCOL_MAX_VERSION;
export const UI_DELTA_CAPABILITY = "serverDelta" as const;
export const UI_MARKDOWN_EDITOR_CAPABILITY = "markdownEditor" as const;
export const UI_MARKDOWN_COMMAND_HINT_CAPABILITY = "markdownCommandHint" as const;
export const UI_MENU_CAPABILITY = "menu" as const;
export const UI_MENU_ANCHOR_CAPABILITY = "menuAnchor" as const;
export const UI_MEDIA_CAPABILITY = "media" as const;
export const UI_PAGE_CAPABILITY = "page" as const;
export const UI_LIST_CAPABILITY = "list" as const;
export const UI_LIST_ITEM_CAPABILITY = "listItem" as const;
export const UI_LIST_ITEM_METADATA_CAPABILITY = "listItemMetadata" as const;
export const UI_LIST_ITEM_ACTIVATE_CAPABILITY = "listItemActivate" as const;
export const UI_LIST_ITEM_PRESENTATION_CAPABILITY = "listItemPresentation" as const;
export const UI_LIST_ITEM_STYLED_TEXT_CAPABILITY = "listItemStyledText" as const;
export const UI_LIST_ITEM_ROLE_CAPABILITY = "listItemRole" as const;
export const UI_LIST_SELECTION_CAPABILITY = "listSelection" as const;
export const UI_STATUS_SYMBOL_CAPABILITY = "statusSymbol" as const;
export const UI_BADGE_CAPABILITY = "badge" as const;
export const UI_SPARKLINE_CAPABILITY = "sparkline" as const;
export const UI_BAR_CHART_CAPABILITY = "barChart" as const;
export const UI_LINE_CHART_CAPABILITY = "lineChart" as const;
export const UI_GAUGE_CAPABILITY = "gauge" as const;
export const UI_TOGGLE_CAPABILITY = "toggle" as const;
export const UI_INPUT_CAPABILITY = "input" as const;
export const UI_BUTTON_CAPABILITY = "button" as const;
export const UI_PAGE_BACK_CAPABILITY = "pageBack" as const;
export const UI_FOOTER_ACTIONS_CAPABILITY = "footerActions" as const;
export const UI_CONTENT_CAPABILITY = "content" as const;
export const UI_CONTENT_SELECTION_CAPABILITY = "contentSelection" as const;
export const UI_SURFACE_CAPABILITY = "surface" as const;
export const UI_CANVAS_PAGE_CAPABILITY = "canvasPage" as const;
export const UI_TREE_CAPABILITY = "tree" as const;
export const UI_TREE_HIERARCHY_CAPABILITY = "treeHierarchy" as const;
export const UI_TREE_FILTER_CAPABILITY = "treeFilter" as const;
export const UI_TREE_PARENT_CAPABILITY = "treeParent" as const;
export const UI_TEXT_BOX_CAPABILITY = "textBox" as const;
/** Built-in renderers that need no Host-injected presenter adapter. */
export const UI_COMPONENT_CAPABILITIES = [
  UI_MARKDOWN_EDITOR_CAPABILITY,
  UI_MARKDOWN_COMMAND_HINT_CAPABILITY,
  UI_MENU_CAPABILITY,
  UI_MENU_ANCHOR_CAPABILITY,
  UI_MEDIA_CAPABILITY,
  UI_PAGE_CAPABILITY,
  UI_LIST_CAPABILITY,
  UI_LIST_ITEM_CAPABILITY,
  UI_LIST_ITEM_METADATA_CAPABILITY,
  UI_LIST_ITEM_ACTIVATE_CAPABILITY,
  UI_LIST_ITEM_PRESENTATION_CAPABILITY,
  UI_LIST_ITEM_STYLED_TEXT_CAPABILITY,
  UI_LIST_ITEM_ROLE_CAPABILITY,
  UI_LIST_SELECTION_CAPABILITY,
  UI_STATUS_SYMBOL_CAPABILITY,
  UI_BADGE_CAPABILITY,
  UI_SPARKLINE_CAPABILITY,
  UI_BAR_CHART_CAPABILITY,
  UI_LINE_CHART_CAPABILITY,
  UI_GAUGE_CAPABILITY,
  UI_TOGGLE_CAPABILITY,
  UI_INPUT_CAPABILITY,
  UI_BUTTON_CAPABILITY,
  UI_PAGE_BACK_CAPABILITY,
  UI_FOOTER_ACTIONS_CAPABILITY,
  UI_CONTENT_CAPABILITY,
  UI_CONTENT_SELECTION_CAPABILITY,
  UI_TREE_CAPABILITY,
  UI_TREE_HIERARCHY_CAPABILITY,
  UI_TREE_FILTER_CAPABILITY,
  UI_TREE_PARENT_CAPABILITY,
  UI_TEXT_BOX_CAPABILITY,
] as const;
export const MAX_INLINE_MEDIA_BYTES = 256 * 1024;

export interface AppMetadata {
  id: string;
  name: string;
  version: string;
  description?: string;
}

export interface UiParticipant {
  id: string;
  kind?: "human" | "agent" | "service";
  sourceSessionId?: string;
  displayName?: string;
  color?: string;
  grants?: string[];
}

export interface UiRendererMetadata {
  id: string;
  kind: string;
  capabilities?: string[];
}

export interface UiRendererState {
  rendererVisible: boolean;
  terminalVisible: boolean;
}

/** Scoped local attachment. Never expose its participantToken to browser code. */
export interface UiAttach {
  type: "attach";
  protocol: typeof UI_PROTOCOL_NAME;
  minProtocolVersion: number;
  maxProtocolVersion: number;
  participantToken: string;
  clientId: string;
  renderer: UiRendererMetadata;
  viewId: string;
  expectedAppInstanceId?: string;
  lastSeenRevision?: number;
  state?: UiRendererState;
}

export interface UiAttached {
  type: "attached";
  protocol: typeof UI_PROTOCOL_NAME;
  protocolVersion: number;
  minProtocolVersion: number;
  maxProtocolVersion: number;
  app: AppMetadata;
  appInstanceId: string;
  participantId: string;
  clientId: string;
  rendererId: string;
  viewId: string;
  resumed: boolean;
  currentRevision?: number;
}

export interface TextPosition {
  line: number;
  utf16Column: number;
}

export interface TextRange {
  start: TextPosition;
  end: TextPosition;
}

export interface TextSelection {
  anchor: TextPosition;
  head: TextPosition;
}

export interface TextEdit {
  range: TextRange;
  text: string;
}

export type MarkdownPresentation = "source" | "preview" | "split";

export type MenuItemRole = "default" | "danger";
export type MenuAnchor = "control" | "caret" | "pointer";
export type MenuPresentation = "popup" | "context";

export interface MenuItemSpec {
  id: string;
  label: string;
  action: string;
  hint?: string;
  disabled?: boolean;
  role?: MenuItemRole;
}

export interface MenuSpec {
  label: string;
  presentation?: MenuPresentation;
  anchor?: MenuAnchor;
  items: MenuItemSpec[];
  selectedId?: string;
  dismiss?: string;
}

export interface MenuNode extends MenuSpec {
  id: string;
  type: "menu";
}

export interface MarkdownEditorActions {
  replaceRange?: string;
  setSelection?: string;
  save?: string;
  undo?: string;
  redo?: string;
  setPresentation?: string;
  openMenu?: string;
}

export type MarkdownCommandHintVisibility = "cursorOnEmptyLineOutsideCodeFence";

export interface MarkdownCommandHint {
  text: string;
  visibility: MarkdownCommandHintVisibility;
}

export type MarkdownMenuTrigger = "slash" | "palette";

export interface MarkdownEditorNode {
  id: string;
  type: "markdownEditor";
  text: string;
  selection: TextSelection;
  presentation?: MarkdownPresentation;
  readOnly?: boolean;
  dirty?: boolean;
  placeholder?: string;
  commandHint?: MarkdownCommandHint;
  title?: string;
  /** Action (kind `cancel`) behind the title's back chevron. */
  back?: string;
  actions?: MarkdownEditorActions;
  insertMenu?: MenuSpec;
  contextMenu?: MenuSpec;
  footer?: FooterActionsSpec;
}

export type FooterActionRole = "default" | "danger";

export interface FooterActionSpec {
  id: string;
  label: string;
  action: string;
  accelerator?: string;
  role?: FooterActionRole;
  disabled?: boolean;
  /** In progress; the terminal shows a braille spinner beside the label. */
  busy?: boolean;
}

export interface FooterActionsSpec {
  actions: FooterActionSpec[];
}

export type MediaFit = "contain" | "cover" | "fill";

export interface MediaPathSource {
  kind: "path";
  path: string;
}

export interface MediaInlineSource {
  kind: "inline";
  mediaType: string;
  base64: string;
}

export interface MediaBlobSource {
  kind: "blob";
  sha256: string;
  mediaType: string;
  byteLength: number;
}

export type MediaSource = MediaPathSource | MediaInlineSource | MediaBlobSource;

export interface MediaPixelSize {
  w: number;
  h: number;
}

export interface MediaCellSize {
  w?: number;
  h?: number;
}

export interface MediaPointSize {
  w?: number;
  h?: number;
}

export type TextBoxTitlePosition = "topLeft" | "topRight" | "bottomLeft" | "bottomRight";
export type TextBoxSubmitMode = "enter" | "never";

export interface TextBoxTitle {
  text: string;
  position: TextBoxTitlePosition;
}

export interface TextBoxKeyHint {
  key: string;
  label: string;
}

export interface TextBoxBusy {
  label: string;
  elapsedMs?: number;
  rightMeta?: string;
}

export interface TextBoxActions {
  setText?: string;
  submit?: string;
}

/** Closed multi-line text box root: plain field or chat-style prompt bar. */
export interface TextBoxNode {
  id: string;
  type: "textBox";
  text?: string;
  placeholder?: string;
  prompt?: string;
  titles?: TextBoxTitle[];
  hints?: TextBoxKeyHint[];
  busy?: TextBoxBusy;
  submitMode?: TextBoxSubmitMode;
  minRows?: number;
  maxRows?: number;
  actions?: TextBoxActions;
}

export interface MediaNode {
  id: string;
  type: "media";
  source: MediaSource;
  intrinsic: MediaPixelSize;
  cells?: MediaCellSize;
  points?: MediaPointSize;
  fit?: MediaFit;
  alt: string;
  activate?: string;
}

/** Opaque Host route; never a socket path, URL, credential, or USRF header. */
export interface SurfaceReference {
  sessionId: string;
  streamId: string;
}

export interface SurfaceCellSize {
  w?: number;
  h?: number;
}

export interface SurfacePointSize {
  w?: number;
  h?: number;
}

export interface SurfaceViewportSize {
  w: number;
  h: number;
}

export type SurfaceBackground =
  | { kind: "transparent" }
  | { kind: "solid"; color: string };

export type SurfaceInputPolicy = "none" | "pointer" | "pointerAndKeyboard";

/** Reference-only canvas leaf. Scene/resource bytes stay on USRF. */
export interface SurfaceNode {
  id: string;
  type: "surface";
  reference: SurfaceReference;
  cells?: SurfaceCellSize;
  points?: SurfacePointSize;
  background?: SurfaceBackground;
  inputPolicy?: SurfaceInputPolicy;
}

export type ButtonRole = "default" | "primary" | "destructive";

export interface ButtonSpec {
  type: "button";
  id: string;
  label: string;
  action: string;
  role?: ButtonRole;
}

/** Named, closed Surface slot inside CanvasPage. */
export interface CanvasSurfaceSpec {
  id: string;
  reference: SurfaceReference;
  cells?: SurfaceCellSize;
  points?: SurfacePointSize;
  background?: SurfaceBackground;
  inputPolicy?: SurfaceInputPolicy;
}

export type CanvasControl = ButtonSpec | UnsupportedComponentSlot;

export interface CanvasPageNode {
  id: string;
  type: "canvasPage";
  title: string;
  surface: CanvasSurfaceSpec;
  controls: CanvasControl[];
}

export interface ToggleSpec {
  type: "toggle";
  id: string;
  label: string;
  value: boolean;
  setValue: string;
  /** `completion` strikes the row when done; `setting` is a plain switch. */
  role?: "completion" | "setting";
}

export interface CheckmarkSpec {
  type: "checkmark";
  id: string;
  label: string;
  value: boolean;
  setValue: string;
}

export interface DisclosureSpec {
  type: "disclosure";
}

export type ListItemTone = "default" | "muted" | "accent" | "info" | "success"
  | "warning" | "danger";
export type ListItemEmphasis = "regular" | "strong";
export type ListItemActionRole = "default" | "destructive";
export type ListItemPrimaryRole = "static" | "toggle" | "checkmark" | "disclosure"
  | "command" | "destructive";
export type ListPageBehavior = "selection" | "scroll";

export interface ListItemTextRun {
  text: string;
  tone?: ListItemTone;
  emphasis?: ListItemEmphasis;
}

export interface StatusSymbolSpec {
  type: "status";
  symbol: string;
  label: string;
  tone?: ListItemTone;
  emphasis?: ListItemEmphasis;
  preserveToneWhenSelected?: boolean;
}

export interface BadgeSpec {
  type: "badge";
  text: string;
  tone?: ListItemTone;
}

export interface SparklineSpec {
  type: "sparkline";
  id: string;
  series: number[];
  min?: number;
  max?: number;
  caption?: string;
  unit?: string;
  accessibilityText: string;
  activate?: string;
}

export type BarChartEmphasis = "default" | "accent" | "danger";

export interface BarChartBar {
  label: string;
  value: number;
  valueCaption?: string;
  emphasis?: BarChartEmphasis;
}

export interface BarChartSpec {
  type: "barChart";
  id: string;
  bars: BarChartBar[];
  accessibilityText: string;
  activate?: string;
}

export interface LineChartPoint {
  x: number;
  y: number;
}

export interface LineChartSeries {
  name: string;
  points: LineChartPoint[];
}

export interface LineChartBounds {
  min: number;
  max: number;
}

export interface LineChartAxis {
  bounds?: LineChartBounds;
  label?: string;
}

export interface LineChartSpec {
  type: "lineChart";
  id: string;
  series: LineChartSeries[];
  xAxis?: LineChartAxis;
  yAxis?: LineChartAxis;
  accessibilityText: string;
  activate?: string;
}

export interface GaugeSpec {
  type: "gauge";
  id: string;
  ratio: number;
  label: string;
  caption?: string;
  accessibilityText: string;
  activate?: string;
}

export interface UnsupportedComponentSlot {
  type: string;
  [field: string]: unknown;
}

export type ListItemSlot = ToggleSpec | StatusSymbolSpec | BadgeSpec | SparklineSpec | GaugeSpec
  | DisclosureSpec | CheckmarkSpec | UnsupportedComponentSlot;

export type ListRowLayout = { type: "inline" } | { type: "stacked" }
  | { type: "auto"; stackBelowWidth: number };

/** Full-width band above or below a ListItem's text rows. */
export type ListItemBand = GaugeSpec | SparklineSpec
  | { type: "text"; text: string; tone?: ListItemTone }
  | { type: "divider" };

/** Media column spanning every row of a ListItem. `spec` is the real image. */
export interface ListItemMedia {
  side?: "leading" | "trailing";
  width: number;
  glyph?: string;
  tone?: ListItemTone;
  spec?: Omit<MediaNode, "id" | "type">;
}

// TODO: the web PageRenderer still draws ListItems on one line; `top`,
// `bottom`, `media`, and `rowLayout` are typed for wire compatibility and
// currently ignored when rendering.
export interface ListItemSpec {
  id: string;
  label: string;
  labelRuns?: ListItemTextRun[];
  labelTone?: ListItemTone;
  emphasis?: ListItemEmphasis;
  detail?: string;
  detailRuns?: ListItemTextRun[];
  value?: string;
  valueRuns?: ListItemTextRun[];
  valueTone?: ListItemTone;
  valueMinWidth?: number;
  done?: boolean;
  busy?: boolean;
  leading?: ListItemSlot;
  trailing?: ListItemSlot;
  accessory?: ListItemSlot;
  top?: ListItemBand;
  bottom?: ListItemBand;
  media?: ListItemMedia;
  /** Passive separator row; the label is an optional caption. */
  divider?: boolean;
  delete?: string;
  activate?: string;
  actionRole?: ListItemActionRole;
}

export interface ListSpec {
  type: "list";
  id: string;
  items: ListItemSpec[];
  emptyMessage?: string;
  selectedId?: string;
  select?: string;
  scrollPadding?: number;
  pageOverlap?: number;
  pageBehavior?: ListPageBehavior;
  spacePagesDown?: boolean;
  rowLayout?: ListRowLayout;
  contextMenu?: MenuSpec;
}

export type ContentFont = "body" | "monospace";
export type ContentTone = "default" | "muted" | "accent" | "info" | "success" | "warning"
  | "danger";
export type ContentEmphasis = "regular" | "strong" | "italic";
export type ContentLineTone = "default" | "muted" | "header" | "added" | "removed";

export interface ContentRun {
  text: string;
  tone?: ContentTone;
  emphasis?: ContentEmphasis;
}

export interface ContentLine {
  id: string;
  runs: ContentRun[];
  tone?: ContentLineTone;
}

export interface ContentSelection {
  anchorId: string;
  headId: string;
}

export interface ContentSpec {
  type: "content";
  id: string;
  label: string;
  lines: ContentLine[];
  wrap?: boolean;
  font?: ContentFont;
  emptyMessage?: string;
  selection?: ContentSelection;
  select?: string;
  contextMenu?: MenuSpec;
}

export interface InputSpec {
  type: "input";
  id: string;
  label: string;
  value?: string;
  placeholder?: string;
  setValue?: string;
  submit?: string;
}

export type PageBodySpec = ListSpec | ContentSpec | SparklineSpec | BarChartSpec | LineChartSpec
  | GaugeSpec | UnsupportedComponentSlot;

export interface PageNode {
  id: string;
  type: "page";
  title: string;
  back?: string;
  header?: InputSpec | UnsupportedComponentSlot;
  body: PageBodySpec;
  footer?: FooterActionsSpec;
}

export type TreePresentation = "drillDown" | "outline";
export type TreeItemKind = "parent" | "directory" | "file";
export type TreeChildState = "loaded" | "unloaded" | "loading";

export interface TreeItem {
  id: string;
  label: string;
  kind: TreeItemKind;
  /** Muted secondary text after the label. */
  detail?: string;
  hidden?: boolean;
  symlink?: boolean;
  childState?: TreeChildState;
  expanded?: boolean;
  children?: TreeItem[];
}

export interface TreeFilter {
  id: string;
  label: string;
  value?: string;
  placeholder?: string;
  setValue: string;
}

export interface TreeActions {
  select: string;
  open: string;
  parent: string;
  setExpanded?: string;
}

export interface TreePrimaryAction {
  id: string;
  label: string;
  action: string;
  role?: ButtonRole;
}

export interface TreeNode {
  id: string;
  type: "tree";
  label: string;
  location: string;
  presentation?: TreePresentation;
  filter?: TreeFilter;
  items: TreeItem[];
  selectedId?: string;
  emptyMessage?: string;
  primaryAction?: TreePrimaryAction;
  contextMenu?: MenuSpec;
  actions: TreeActions;
  footer?: FooterActionsSpec;
}

/** Opaque root retained only so the session can request terminal fallback. */
export interface UnsupportedUiNode {
  id: string;
  type: string;
  [field: string]: unknown;
}

export type UiNode = CanvasPageNode | MarkdownEditorNode | MediaNode | MenuNode | PageNode | SurfaceNode
  | TextBoxNode
  | TreeNode
  | UnsupportedUiNode;

export function isTextBoxNode(node: UiNode): node is TextBoxNode {
  return node.type === "textBox";
}

export function isCanvasPageNode(node: UiNode): node is CanvasPageNode {
  return node.type === "canvasPage";
}

export function isMarkdownEditorNode(node: UiNode): node is MarkdownEditorNode {
  return node.type === "markdownEditor";
}

/** Pure interpretation of the closed Rust command-hint visibility rule. */
export function isMarkdownCommandHintVisible(editor: MarkdownEditorNode): boolean {
  const hint = editor.commandHint;
  const presentation = editor.presentation ?? "source";
  if (hint === undefined
    || presentation === "preview"
    || editor.selection.anchor.line !== editor.selection.head.line
    || editor.selection.anchor.utf16Column !== editor.selection.head.utf16Column
    || editor.insertMenu !== undefined
    || (editor.text === "" && (editor.placeholder ?? "") !== "")) return false;
  const lines = editor.text.split("\n");
  const line = editor.selection.head.line;
  if (lines[line] !== "") return false;
  switch (hint.visibility) {
    case "cursorOnEmptyLineOutsideCodeFence": {
      let insideFence = false;
      for (let index = 0; index <= line; index += 1) {
        if (lines[index]!.trimStart().startsWith("```")) {
          if (index === line) return false;
          insideFence = !insideFence;
        }
      }
      return !insideFence;
    }
  }
}

/** Closed text triggers for the App-owned Menu intent. */
export function markdownMenuTriggerForTextInput(
  editor: MarkdownEditorNode,
  input: string,
): MarkdownMenuTrigger | undefined {
  if (editor.readOnly === true
    || editor.insertMenu !== undefined
    || editor.actions?.openMenu === undefined) return undefined;
  if (input === "/") return "slash";
  if (input === "\\") return "palette";
  return undefined;
}

export function isMediaNode(node: UiNode): node is MediaNode {
  return node.type === "media";
}

export function isMenuNode(node: UiNode): node is MenuNode {
  return node.type === "menu";
}

export function isPageNode(node: UiNode): node is PageNode {
  return node.type === "page";
}

export function isSurfaceNode(node: UiNode): node is SurfaceNode {
  return node.type === "surface";
}

export function isTreeNode(node: UiNode): node is TreeNode {
  return node.type === "tree";
}

export function isButtonControl(control: CanvasControl): control is ButtonSpec {
  return control.type === "button";
}

export function canvasSurfaceNode(surface: CanvasSurfaceSpec): SurfaceNode {
  return { ...surface, type: "surface" };
}

export function isToggleSlot(slot: ListItemSlot): slot is ToggleSpec {
  return slot.type === "toggle";
}

export function isStatusSlot(slot: ListItemSlot): slot is StatusSymbolSpec {
  return slot.type === "status";
}

export function isBadgeSlot(slot: ListItemSlot): slot is BadgeSpec {
  return slot.type === "badge";
}

export function isSparklineSlot(slot: ListItemSlot): slot is SparklineSpec {
  return slot.type === "sparkline";
}

export function isGaugeSlot(slot: ListItemSlot): slot is GaugeSpec {
  return slot.type === "gauge" && typeof (slot as Partial<GaugeSpec>).ratio === "number";
}

export function isDisclosureSlot(slot: ListItemSlot): slot is DisclosureSpec {
  return slot.type === "disclosure";
}

export function isCheckmarkSlot(slot: ListItemSlot): slot is CheckmarkSpec {
  return slot.type === "checkmark";
}

export function isKnownListItemSlot(
  slot: ListItemSlot,
): slot is ToggleSpec | StatusSymbolSpec | BadgeSpec | SparklineSpec | DisclosureSpec
  | GaugeSpec | CheckmarkSpec {
  return isToggleSlot(slot) || isStatusSlot(slot) || isBadgeSlot(slot)
    || isSparklineSlot(slot) || isGaugeSlot(slot) || isDisclosureSlot(slot)
    || isCheckmarkSlot(slot);
}

/** Authoritative cross-renderer domain: inferred bounds include zero. */
export function resolvedSparklineBounds(sparkline: SparklineSpec): [number, number] {
  let seriesMinimum = Number.POSITIVE_INFINITY;
  let seriesMaximum = Number.NEGATIVE_INFINITY;
  for (const value of sparkline.series) {
    seriesMinimum = Math.min(seriesMinimum, value);
    seriesMaximum = Math.max(seriesMaximum, value);
  }
  const lower = sparkline.min ?? Math.min(seriesMinimum, 0);
  let upper = sparkline.max ?? Math.max(seriesMaximum, 0);
  if (lower === upper) upper = lower + 1;
  return [lower, upper];
}

export function normalizedSparklineSeries(sparkline: SparklineSpec): number[] {
  const [lower, upper] = resolvedSparklineBounds(sparkline);
  const range = upper - lower;
  return sparkline.series.map((value) => Math.min(Math.max((value - lower) / range, 0), 1));
}

export function isBarChartSpec(slot: PageBodySpec): slot is BarChartSpec {
  return slot.type === "barChart" && Array.isArray((slot as Partial<BarChartSpec>).bars);
}

export function isLineChartSpec(slot: PageBodySpec): slot is LineChartSpec {
  return slot.type === "lineChart" && Array.isArray((slot as Partial<LineChartSpec>).series);
}

export function isGaugeSpec(slot: PageBodySpec): slot is GaugeSpec {
  return slot.type === "gauge" && typeof (slot as Partial<GaugeSpec>).ratio === "number";
}

export function isSparklineBodySpec(slot: PageBodySpec): slot is SparklineSpec {
  return slot.type === "sparkline" && Array.isArray((slot as Partial<SparklineSpec>).series);
}

export function normalizedBarChartValues(chart: BarChartSpec): number[] {
  let maximum = 0;
  for (const bar of chart.bars) maximum = Math.max(maximum, bar.value);
  maximum = Math.max(maximum, 1);
  return chart.bars.map((bar) => Math.min(Math.max(bar.value / maximum, 0), 1));
}

export function resolvedLineChartBounds(
  chart: LineChartSpec,
  axis: "x" | "y",
): [number, number] {
  const spec = axis === "x" ? chart.xAxis : chart.yAxis;
  if (spec?.bounds !== undefined) return [spec.bounds.min, spec.bounds.max];
  let lower = Number.POSITIVE_INFINITY;
  let upper = Number.NEGATIVE_INFINITY;
  for (const series of chart.series) {
    for (const point of series.points) {
      const value = axis === "x" ? point.x : point.y;
      lower = Math.min(lower, value);
      upper = Math.max(upper, value);
    }
  }
  if (lower === upper) upper = lower + 1;
  return [lower, upper];
}

export function gaugePercentageLabel(gauge: GaugeSpec): string {
  return gauge.label + "  " + gaugeValueLabel(gauge);
}

export function gaugePercentageValueLabel(gauge: GaugeSpec): string {
  return String(Math.round(gauge.ratio * 100)) + "%";
}

export function gaugeValueLabel(gauge: GaugeSpec): string {
  return gauge.caption ?? gaugePercentageValueLabel(gauge);
}

export function listItemPrimaryRole(item: ListItemSpec): ListItemPrimaryRole {
  const slots = [item.leading, item.trailing, item.accessory].filter(
    (slot): slot is ListItemSlot => slot !== undefined,
  );
  if (slots.some(isToggleSlot)) return "toggle";
  if (slots.some(isCheckmarkSlot)) return "checkmark";
  if (slots.some(isDisclosureSlot)) return "disclosure";
  if (slots.some((slot) => isSparklineSlot(slot) && slot.activate !== undefined)) {
    return "command";
  }
  if (slots.some((slot) => isGaugeSlot(slot) && slot.activate !== undefined)) return "command";
  if (item.activate !== undefined) {
    return item.actionRole === "destructive" ? "destructive" : "command";
  }
  return "static";
}

export function isListSpec(
  slot: PageBodySpec,
): slot is ListSpec {
  return slot.type === "list" && Array.isArray(slot.items);
}

export function isContentSpec(
  slot: PageBodySpec,
): slot is ContentSpec {
  return slot.type === "content" && Array.isArray(slot.lines);
}

export function isInputSpec(slot: InputSpec | UnsupportedComponentSlot): slot is InputSpec {
  return slot.type === "input" && typeof slot.id === "string";
}

/** A Page is renderable only when every named slot uses this wrapper's vocabulary. */
export function isRenderablePageNode(node: UiNode): node is PageNode & {
  header?: InputSpec;
  body: ListSpec;
} {
  if (!isPageNode(node) || !isListSpec(node.body)) return false;
  if (node.header !== undefined && !isInputSpec(node.header)) return false;
  return isValidFooterActions(node.footer)
    && (node.body.contextMenu === undefined || isValidMenu(node.body.contextMenu))
    && node.body.items.every((item) => [item.leading, item.trailing, item.accessory]
      .every((slot) => slot === undefined || isKnownListItemSlot(slot)));
}

export function isRenderableContentPageNode(node: UiNode): node is PageNode & {
  header?: InputSpec;
  body: ContentSpec;
} {
  return isPageNode(node)
    && isContentSpec(node.body)
    && (node.header === undefined || isInputSpec(node.header))
    && isValidContent(node.body)
    && isValidFooterActions(node.footer);
}

export function isRenderableChartPageNode(node: UiNode): node is PageNode & {
  header?: InputSpec;
  body: SparklineSpec | BarChartSpec | LineChartSpec | GaugeSpec;
} {
  return isPageNode(node)
    && (isSparklineBodySpec(node.body) || isBarChartSpec(node.body)
      || isLineChartSpec(node.body) || isGaugeSpec(node.body))
    && (node.header === undefined || isInputSpec(node.header))
    && isValidFooterActions(node.footer);
}

/** Capability required for a known root, or undefined for an unknown kind. */
export function uiNodeCapability(node: UiNode): string | undefined {
  return uiNodeCapabilities(node)?.[0];
}

/** All capabilities needed for a known closed tree, or undefined for fallback. */
export function uiNodeCapabilities(node: UiNode): readonly string[] | undefined {
  if (isCanvasPageNode(node)) {
    if (!node.controls.every(isButtonControl)) return undefined;
    const capabilities: string[] = [UI_CANVAS_PAGE_CAPABILITY, UI_SURFACE_CAPABILITY];
    if (node.controls.length > 0) capabilities.push(UI_BUTTON_CAPABILITY);
    return capabilities;
  }
  if (isMarkdownEditorNode(node)) {
    if ((node.insertMenu !== undefined && !isValidMenu(node.insertMenu))
      || (node.contextMenu !== undefined && !isValidMenu(node.contextMenu))
      || (node.commandHint !== undefined
        && (!isValidMarkdownCommandHint(node.commandHint)
          || node.actions?.openMenu === undefined))
      || !isValidFooterActions(node.footer)) return undefined;
    const capabilities: string[] = [UI_MARKDOWN_EDITOR_CAPABILITY];
    if (node.commandHint !== undefined) {
      capabilities.push(UI_MARKDOWN_COMMAND_HINT_CAPABILITY);
    }
    if (node.insertMenu !== undefined || node.contextMenu !== undefined) {
      capabilities.push(UI_MENU_CAPABILITY, UI_MENU_ANCHOR_CAPABILITY);
    }
    if ((node.footer?.actions.length ?? 0) > 0) {
      capabilities.push(UI_FOOTER_ACTIONS_CAPABILITY);
    }
    return capabilities;
  }
  if (isMediaNode(node)) return [UI_MEDIA_CAPABILITY];
  if (isMenuNode(node)) {
    return isValidMenu(node) ? [UI_MENU_CAPABILITY, UI_MENU_ANCHOR_CAPABILITY] : undefined;
  }
  if (isSurfaceNode(node)) return [UI_SURFACE_CAPABILITY];
  if (isTreeNode(node)) {
    if (!isValidTree(node)) return undefined;
    const capabilities: string[] = [UI_TREE_CAPABILITY];
    const flat = flattenTreeItems(node.items);
    if ((node.presentation ?? "drillDown") === "outline"
      || flat.some((item) => (item.children?.length ?? 0) > 0)) {
      capabilities.push(UI_TREE_HIERARCHY_CAPABILITY);
    }
    if (node.filter !== undefined) capabilities.push(UI_TREE_FILTER_CAPABILITY);
    if (flat.some((item) => item.kind === "parent")) {
      capabilities.push(UI_TREE_PARENT_CAPABILITY);
    }
    if (node.primaryAction !== undefined) capabilities.push(UI_BUTTON_CAPABILITY);
    if (node.contextMenu !== undefined) {
      capabilities.push(UI_MENU_CAPABILITY, UI_MENU_ANCHOR_CAPABILITY);
    }
    if ((node.footer?.actions.length ?? 0) > 0) {
      capabilities.push(UI_FOOTER_ACTIONS_CAPABILITY);
    }
    return capabilities;
  }
  if (isRenderableChartPageNode(node)) {
    const capability = isSparklineBodySpec(node.body)
      ? UI_SPARKLINE_CAPABILITY
      : isBarChartSpec(node.body)
        ? UI_BAR_CHART_CAPABILITY
        : isLineChartSpec(node.body)
          ? UI_LINE_CHART_CAPABILITY
          : UI_GAUGE_CAPABILITY;
    const capabilities: string[] = [UI_PAGE_CAPABILITY];
    if ((node.footer?.actions.length ?? 0) > 0) {
      capabilities.push(UI_FOOTER_ACTIONS_CAPABILITY);
    }
    capabilities.push(capability);
    if (node.header !== undefined) capabilities.push(UI_INPUT_CAPABILITY);
    if (node.back !== undefined) capabilities.push(UI_PAGE_BACK_CAPABILITY);
    return capabilities;
  }
  if (!isRenderablePageNode(node) && !isRenderableContentPageNode(node)) return undefined;
  const capabilities: string[] = [UI_PAGE_CAPABILITY];
  if ((node.footer?.actions.length ?? 0) > 0) {
    capabilities.push(UI_FOOTER_ACTIONS_CAPABILITY);
  }
  if (isRenderableContentPageNode(node)) {
    capabilities.push(UI_CONTENT_CAPABILITY);
    if (node.header !== undefined) capabilities.push(UI_INPUT_CAPABILITY);
    if (node.back !== undefined) capabilities.push(UI_PAGE_BACK_CAPABILITY);
    if (node.body.selection !== undefined || node.body.select !== undefined) {
      capabilities.push(UI_CONTENT_SELECTION_CAPABILITY);
    }
    if (node.body.contextMenu !== undefined) {
      capabilities.push(UI_MENU_CAPABILITY, UI_MENU_ANCHOR_CAPABILITY);
    }
    return capabilities;
  }
  if (!isRenderablePageNode(node)) return undefined;
  capabilities.push(UI_LIST_CAPABILITY, UI_LIST_ITEM_CAPABILITY);
  if (node.header !== undefined) capabilities.push(UI_INPUT_CAPABILITY);
  if (node.back !== undefined) capabilities.push(UI_PAGE_BACK_CAPABILITY);
  if (node.body.items.some((item) => item.detail !== undefined || item.value !== undefined)) {
    capabilities.push(UI_LIST_ITEM_METADATA_CAPABILITY);
  }
  if (node.body.items.some((item) => (item.labelRuns?.length ?? 0) > 0
    || (item.detailRuns?.length ?? 0) > 0
    || (item.valueRuns?.length ?? 0) > 0)) {
    capabilities.push(UI_LIST_ITEM_STYLED_TEXT_CAPABILITY);
  }
  if (node.body.items.some((item) => item.activate !== undefined)) {
    capabilities.push(UI_LIST_ITEM_ACTIVATE_CAPABILITY);
  }
  if (node.body.items.some((item) => listItemPrimaryRole(item) !== "static")) {
    capabilities.push(UI_LIST_ITEM_ROLE_CAPABILITY);
  }
  if (node.body.items.some((item) => [item.leading, item.trailing, item.accessory]
    .some((slot) => slot?.type === "toggle"))) {
    capabilities.push(UI_TOGGLE_CAPABILITY);
  }
  if (node.body.items.some((item) => item.busy === true
    || (item.labelTone !== undefined && item.labelTone !== "default")
    || (item.valueTone !== undefined && item.valueTone !== "muted")
    || (item.emphasis !== undefined && item.emphasis !== "regular")
    || item.valueMinWidth !== undefined
    || [item.leading, item.trailing, item.accessory]
      .some((slot) => slot?.type === "status" || slot?.type === "badge"))) {
    capabilities.push(UI_LIST_ITEM_PRESENTATION_CAPABILITY);
  }
  if (node.body.items.some((item) => [item.leading, item.trailing, item.accessory]
    .some((slot) => slot?.type === "status"))) {
    capabilities.push(UI_STATUS_SYMBOL_CAPABILITY);
  }
  if (node.body.items.some((item) => [item.leading, item.trailing, item.accessory]
    .some((slot) => slot?.type === "badge"))) {
    capabilities.push(UI_BADGE_CAPABILITY);
  }
  if (node.body.items.some((item) => [item.leading, item.trailing, item.accessory]
    .some((slot) => slot?.type === "sparkline"))) {
    capabilities.push(UI_SPARKLINE_CAPABILITY);
  }
  if (node.body.items.some((item) => [item.leading, item.trailing, item.accessory]
    .some((slot) => slot?.type === "gauge"))) {
    capabilities.push(UI_GAUGE_CAPABILITY);
  }
  if (node.body.selectedId !== undefined || node.body.select !== undefined
    || (node.body.scrollPadding ?? 0) !== 0 || (node.body.pageOverlap ?? 1) !== 1
    || (node.body.pageBehavior ?? "selection") !== "selection"
    || node.body.spacePagesDown === true) {
    capabilities.push(UI_LIST_SELECTION_CAPABILITY);
  }
  if (node.body.contextMenu !== undefined) {
    capabilities.push(UI_MENU_CAPABILITY, UI_MENU_ANCHOR_CAPABILITY);
  }
  return capabilities;
}

function isValidContent(content: ContentSpec): boolean {
  if (content.lines.length > 100_000) return false;
  const ids = new Set<string>();
  for (const line of content.lines) {
    if (ids.has(line.id) || line.runs.some((run) => /[\n\r\0]/u.test(run.text))) return false;
    ids.add(line.id);
  }
  if (content.selection !== undefined
    && (!ids.has(content.selection.anchorId) || !ids.has(content.selection.headId)
      || content.select === undefined)) return false;
  return content.contextMenu === undefined || isValidMenu(content.contextMenu);
}

export function isValidFooterActions(footer: FooterActionsSpec | undefined): boolean {
  if (footer === undefined) return true;
  if (!Array.isArray(footer.actions) || footer.actions.length > 100_000) return false;
  const ids = new Set<string>();
  const accelerators = new Set<string>();
  for (const action of footer.actions) {
    if (!portableIdentifier.test(action.id) || !portableIdentifier.test(action.action)
      || new TextEncoder().encode(action.label).length > 4_096
      || action.label.includes("\n") || action.label.includes("\r")
      || ids.has(action.id)
      || (action.role !== undefined && action.role !== "default" && action.role !== "danger")
      || (action.disabled !== undefined && typeof action.disabled !== "boolean")) return false;
    ids.add(action.id);
    if (action.accelerator !== undefined) {
      if (!isFooterAccelerator(action.accelerator) || accelerators.has(action.accelerator)) {
        return false;
      }
      accelerators.add(action.accelerator);
    }
  }
  return true;
}

const portableIdentifier = /^[A-Za-z0-9._:/-]{1,256}$/u;

function isFooterAccelerator(value: string): boolean {
  return value === "escape" || value === "enter" || value === "space"
    || /^ctrl\+[A-Za-z0-9]$/u.test(value)
    || /^[\x21-\x7e]$/u.test(value);
}

export function flattenTreeItems(items: readonly TreeItem[]): TreeItem[] {
  return items.flatMap((item) => [item, ...flattenTreeItems(item.children ?? [])]);
}

function isValidMenu(menu: MenuSpec): boolean {
  if (menu.items.length > 256) return false;
  const ids = new Set(menu.items.map((item) => item.id));
  if (ids.size !== menu.items.length) return false;
  if (menu.selectedId !== undefined) {
    const selected = menu.items.find((item) => item.id === menu.selectedId);
    if (selected === undefined || selected.disabled === true) return false;
  }
  return true;
}

function isValidTree(node: TreeNode): boolean {
  const ids = new Set<string>();
  let count = 0;
  let parents = 0;
  const visit = (items: readonly TreeItem[], depth: number): boolean => {
    if (depth > 32) return false;
    for (const item of items) {
      count += 1;
      if (count > 100_000 || ids.has(item.id) || item.label.includes("\n")
        || item.label.includes("\r")) return false;
      ids.add(item.id);
      const children = item.children ?? [];
      if (item.kind === "parent") {
        parents += 1;
        if (depth !== 0 || children.length > 0 || item.expanded === true) return false;
      } else if (item.kind === "file") {
        if (children.length > 0 || item.expanded === true) return false;
      } else if ((item.childState ?? "loaded") !== "loaded" && children.length > 0) {
        return false;
      }
      if (!visit(children, depth + 1)) return false;
    }
    return true;
  };
  const validItems = visit(node.items, 0);
  const footerIDs = new Set(node.footer?.actions.map((action) => action.id) ?? []);
  return validItems && parents <= 1
    && (node.selectedId === undefined || ids.has(node.selectedId))
    && ((node.presentation ?? "drillDown") !== "outline"
      || node.actions.setExpanded !== undefined)
    && (node.contextMenu === undefined || isValidMenu(node.contextMenu))
    && isValidFooterActions(node.footer)
    && [...footerIDs].every((id) => !ids.has(id));
}

/** Filesystem paths must be translated by the Host before entering a browser. */
export function isBrowserSafeUiNode(node: UiNode): boolean {
  return !isMediaNode(node) || node.source.kind !== "path";
}

export interface UiSnapshot {
  type: "snapshot";
  protocol: typeof UI_PROTOCOL_NAME;
  protocolVersion: number;
  appInstanceId: string;
  clientId: string;
  viewId: string;
  revision: number;
  root: UiNode;
}

export type UiDeltaOperation =
  | { op: "replaceRoot"; root: UiNode }
  | { op: "markdownReplaceRange"; nodeId: string; edit: TextEdit }
  | { op: "markdownSetSelection"; nodeId: string; selection: TextSelection }
  | { op: "markdownSetPresentation"; nodeId: string; presentation: MarkdownPresentation }
  | { op: "markdownSetDirty"; nodeId: string; dirty: boolean }
  | { op: "markdownSetReadOnly"; nodeId: string; readOnly: boolean }
  | { op: "markdownSetTitle"; nodeId: string; title: string | null }
  | { op: "markdownSetPlaceholder"; nodeId: string; placeholder: string }
  | { op: "markdownSetCommandHint"; nodeId: string; commandHint: MarkdownCommandHint | null }
  | { op: "markdownSetActions"; nodeId: string; actions: MarkdownEditorActions }
  | {
    op: "markdownSetMenus";
    nodeId: string;
    insertMenu?: MenuSpec;
    contextMenu?: MenuSpec;
  }
  | { op: "menuSetSelection"; nodeId: string; selectedId: string | null }
  | {
    op: "mediaSetSource";
    nodeId: string;
    source: MediaSource;
    intrinsic: MediaPixelSize;
  }
  | { op: "surfaceSetReference"; nodeId: string; reference: SurfaceReference }
  | { op: "toggleSetValue"; nodeId: string; value: boolean }
  | { op: "checkmarkSetValue"; nodeId: string; value: boolean }
  | {
    op: "sparklineSetData";
    nodeId: string;
    series: number[];
    min: number | null;
    max: number | null;
    caption: string | null;
    unit: string | null;
    accessibilityText: string;
  }
  | {
    op: "barChartSetData";
    nodeId: string;
    bars: BarChartBar[];
    accessibilityText: string;
  }
  | {
    op: "lineChartSetData";
    nodeId: string;
    series: LineChartSeries[];
    xAxis: LineChartAxis;
    yAxis: LineChartAxis;
    accessibilityText: string;
  }
  | {
    op: "gaugeSetData";
    nodeId: string;
    ratio: number;
    label: string;
    caption?: string | null;
    accessibilityText: string;
  }
  | { op: "footerSetActions"; nodeId: string; actions: FooterActionSpec[] }
  | { op: "inputSetValue"; nodeId: string; value: string }
  | { op: "listInsertItem"; listId: string; index: number; item: ListItemSpec }
  | { op: "listSetSelection"; listId: string; selectedId: string | null }
  | { op: "listRemoveItem"; listId: string; itemId: string }
  | { op: "contentSetSelection"; contentId: string; selection: ContentSelection | null }
  | {
    op: "contentSpliceLines";
    contentId: string;
    index: number;
    deleteCount: number;
    lines: ContentLine[];
  }
  | { op: "treeSetSelection"; nodeId: string; selectedId: string | null }
  | { op: "treeSetFilter"; filterId: string; value: string }
  | { op: "treeSetLocation"; nodeId: string; location: string }
  | {
    op: "treeSpliceChildren";
    nodeId: string;
    parentId?: string;
    index: number;
    deleteCount: number;
    items: TreeItem[];
  }
  | {
    op: "treeSetChildState";
    nodeId: string;
    itemId: string;
    childState: TreeChildState;
  }
  | { op: "treeSetExpanded"; nodeId: string; itemId: string; expanded: boolean };

export interface UiDelta {
  type: "delta";
  protocol: typeof UI_PROTOCOL_NAME;
  protocolVersion: number;
  appInstanceId: string;
  clientId: string;
  viewId: string;
  baseRevision: number;
  revision: number;
  operations: UiDeltaOperation[];
}

export type UiEventKind =
  | "activate"
  | "select"
  | "change"
  | "submit"
  | "cancel"
  | "command";

export type UiEventValue =
  | { type: "none" }
  | { type: "bool"; value: boolean }
  | { type: "index"; value: number }
  | { type: "integer"; value: number }
  | { type: "number"; value: number }
  | { type: "text"; value: string }
  | { type: "textList"; value: string[] }
  | { type: "textEdit"; value: TextEdit }
  | { type: "textSelection"; value: TextSelection };

/** Renderer-local action before authenticated session context is applied. */
export interface UiAction {
  nodeId: string;
  action: string;
  kind: UiEventKind;
  value: UiEventValue;
}

export interface UiEvent extends UiAction {
  type: "event";
  protocol: typeof UI_PROTOCOL_NAME;
  protocolVersion: number;
  appInstanceId: string;
  participantId: string;
  clientId: string;
  rendererId: string;
  viewId: string;
  eventId: string;
  baseRevision: number;
}

export type UiAckStatus = "pending" | "applied" | "rejected" | "stale";

export interface UiAck {
  type: "ack";
  protocol: typeof UI_PROTOCOL_NAME;
  protocolVersion: number;
  appInstanceId: string;
  clientId: string;
  rendererId: string;
  viewId: string;
  eventId: string;
  status: UiAckStatus;
  revision: number;
  message?: string;
}

export interface UiLifecycle {
  type: "lifecycle";
  protocol: typeof UI_PROTOCOL_NAME;
  protocolVersion: number;
  appInstanceId: string;
  clientId: string;
  rendererId: string;
  viewId: string;
  state: UiRendererState;
}

export interface UiRequestSnapshot {
  type: "requestSnapshot";
  protocol: typeof UI_PROTOCOL_NAME;
  protocolVersion: number;
  appInstanceId: string;
  clientId: string;
  rendererId: string;
  viewId: string;
}

export interface UiPresenceMember {
  participant: UiParticipant;
  clientId: string;
  renderer: UiRendererMetadata;
  state: UiRendererState;
}

export interface UiPresence {
  type: "presence";
  protocol: typeof UI_PROTOCOL_NAME;
  protocolVersion: number;
  appInstanceId: string;
  viewId: string;
  members: UiPresenceMember[];
}

export interface UiErrorMessage {
  type: "error";
  protocol: typeof UI_PROTOCOL_NAME;
  protocolVersion: number;
  code: string;
  message: string;
}

export type UiMessage =
  | UiAttach
  | UiAttached
  | UiSnapshot
  | UiDelta
  | UiEvent
  | UiAck
  | UiLifecycle
  | UiRequestSnapshot
  | UiPresence
  | UiErrorMessage;

/** Selects the highest protocol version shared with this wrapper. */
export function negotiateUiProtocolVersion(minimum: number, maximum: number): number | undefined {
  if (!Number.isSafeInteger(minimum) || !Number.isSafeInteger(maximum)
    || minimum < 1 || minimum > maximum || maximum > 4_294_967_295) return undefined;
  const sharedMinimum = Math.max(minimum, UI_PROTOCOL_MIN_VERSION);
  const sharedMaximum = Math.min(maximum, UI_PROTOCOL_MAX_VERSION);
  return sharedMinimum <= sharedMaximum ? sharedMaximum : undefined;
}

/**
 * Decodes a known message and intentionally ignores unknown object fields for
 * forward compatibility. Unknown component roots remain opaque so a session
 * can keep its attachment and expose the complete terminal pane. Unknown
 * message, action, and value kinds remain errors.
 */
export function decodeUiMessage(input: string | unknown): UiMessage {
  const value: unknown = typeof input === "string" ? JSON.parse(input) : input;
  const message = record(value, "UI message");
  if (message.protocol !== UI_PROTOCOL_NAME) {
    throw new Error(`Unexpected UI protocol ${String(message.protocol)}`);
  }
  const messageType = message.type;
  if (messageType !== "attach") requireSupportedProtocolVersion(message.protocolVersion);

  switch (messageType) {
    case "attach":
      validateProtocolRange(
        message.minProtocolVersion,
        message.maxProtocolVersion,
        "attach",
      );
      requireString(message.participantToken, "attach.participantToken");
      if (message.participantToken.length > 16_384) {
        throw new Error("attach.participantToken must contain at most 16384 characters");
      }
      requireIdentifier(message.clientId, "attach.clientId");
      validateRenderer(message.renderer, "attach.renderer");
      requireIdentifier(message.viewId, "attach.viewId");
      if (message.expectedAppInstanceId !== undefined) {
        requireIdentifier(message.expectedAppInstanceId, "attach.expectedAppInstanceId");
      }
      if (message.lastSeenRevision !== undefined) {
        requireSafeInteger(message.lastSeenRevision, "attach.lastSeenRevision");
      }
      if (message.state !== undefined) {
        validateRendererState(message.state, "attach.state");
      }
      break;
    case "attached": {
      validateProtocolRange(
        message.minProtocolVersion,
        message.maxProtocolVersion,
        "attached",
      );
      const selectedVersion = requireSupportedProtocolVersion(message.protocolVersion);
      if (selectedVersion < Number(message.minProtocolVersion)
        || selectedVersion > Number(message.maxProtocolVersion)) {
        throw new Error("attached.protocolVersion is outside the advertised server range");
      }
      const app = record(message.app, "attached.app");
      requireString(app.id, "attached.app.id");
      requireString(app.name, "attached.app.name");
      requireString(app.version, "attached.app.version");
      if (app.description !== undefined) {
        requireString(app.description, "attached.app.description", true);
      }
      validateRoute(message, "attached", true);
      requireIdentifier(message.participantId, "attached.participantId");
      if (typeof message.resumed !== "boolean") {
        throw new Error("attached.resumed must be a boolean");
      }
      if (message.currentRevision !== undefined) {
        requireSafeInteger(message.currentRevision, "attached.currentRevision");
      }
      break;
    }
    case "snapshot": {
      requireIdentifier(message.appInstanceId, "snapshot.appInstanceId");
      requireIdentifier(message.clientId, "snapshot.clientId");
      requireIdentifier(message.viewId, "snapshot.viewId");
      requireSafeInteger(message.revision, "snapshot.revision");
      validateNode(message.root, "snapshot.root");
      break;
    }
    case "delta": {
      requireIdentifier(message.appInstanceId, "delta.appInstanceId");
      requireIdentifier(message.clientId, "delta.clientId");
      requireIdentifier(message.viewId, "delta.viewId");
      requireSafeInteger(message.baseRevision, "delta.baseRevision");
      requireSafeInteger(message.revision, "delta.revision");
      if (message.revision <= message.baseRevision) {
        throw new Error("delta.revision must be greater than baseRevision");
      }
      if (!Array.isArray(message.operations)
        || message.operations.length === 0
        || message.operations.length > 4_096) {
        throw new Error("delta.operations must contain 1..=4096 entries");
      }
      for (const [index, operation] of message.operations.entries()) {
        validateDeltaOperation(operation, `delta.operations[${index}]`);
      }
      break;
    }
    case "event":
      validateRoute(message, "event", true);
      requireIdentifier(message.participantId, "event.participantId");
      requireIdentifier(message.eventId, "event.eventId");
      requireSafeInteger(message.baseRevision, "event.baseRevision");
      validateAction(message, "event");
      break;
    case "ack":
      validateRoute(message, "ack", true);
      requireIdentifier(message.eventId, "ack.eventId");
      requireSafeInteger(message.revision, "ack.revision");
      if (!["pending", "applied", "rejected", "stale"].includes(String(message.status))) {
        throw new Error(`Unsupported ack status ${String(message.status)}`);
      }
      if (message.message !== undefined) {
        requireString(message.message, "ack.message", true);
      }
      break;
    case "lifecycle":
      validateRoute(message, "lifecycle", true);
      validateRendererState(message.state, "lifecycle.state");
      break;
    case "requestSnapshot":
      validateRoute(message, "requestSnapshot", true);
      break;
    case "presence": {
      requireIdentifier(message.appInstanceId, "presence.appInstanceId");
      requireIdentifier(message.viewId, "presence.viewId");
      if (!Array.isArray(message.members)) {
        throw new Error("presence.members must be an array");
      }
      for (const [index, memberValue] of message.members.entries()) {
        const member = record(memberValue, `presence.members[${index}]`);
        validateParticipant(member.participant, `presence.members[${index}].participant`);
        requireIdentifier(member.clientId, `presence.members[${index}].clientId`);
        validateRenderer(member.renderer, `presence.members[${index}].renderer`);
        validateRendererState(member.state, `presence.members[${index}].state`);
      }
      break;
    }
    case "error":
      requireIdentifier(message.code, "error.code");
      requireString(message.message, "error.message");
      break;
    default:
      throw new Error(`Unsupported UI message ${String(message.type)}`);
  }
  return value as UiMessage;
}

/** Applies a contiguous server delta and returns the next complete snapshot. */
export function applyUiDelta(snapshot: UiSnapshot, delta: UiDelta): UiSnapshot {
  if (snapshot.protocol !== delta.protocol
    || snapshot.protocolVersion !== delta.protocolVersion
    || snapshot.appInstanceId !== delta.appInstanceId
    || snapshot.clientId !== delta.clientId
    || snapshot.viewId !== delta.viewId) {
    throw new Error("Delta route does not match the current snapshot");
  }
  if (snapshot.revision !== delta.baseRevision || delta.revision <= delta.baseRevision) {
    throw new Error("Delta is not contiguous with the current snapshot");
  }
  if (delta.operations.length === 0 || delta.operations.length > 4_096) {
    throw new Error("Delta must contain 1..=4096 operations");
  }

  let root = snapshot.root;
  for (const operation of delta.operations) {
    root = applyDeltaOperation(root, operation);
  }
  validateNode(root, "delta.result.root");
  return {
    type: "snapshot",
    protocol: delta.protocol,
    protocolVersion: delta.protocolVersion,
    appInstanceId: delta.appInstanceId,
    clientId: delta.clientId,
    viewId: delta.viewId,
    revision: delta.revision,
    root,
  };
}

function applyDeltaOperation(root: UiNode, operation: UiDeltaOperation): UiNode {
  if (operation.op === "replaceRoot") return operation.root;
  if (operation.op === "mediaSetSource") {
    if (root.id !== operation.nodeId || !isMediaNode(root)) {
      throw new Error("Delta targets an unavailable Media node");
    }
    return {
      ...root,
      source: operation.source,
      intrinsic: operation.intrinsic,
    };
  }
  if (operation.op === "surfaceSetReference") {
    if (isSurfaceNode(root) && root.id === operation.nodeId) {
      return { ...root, reference: operation.reference };
    }
    if (isCanvasPageNode(root) && root.surface.id === operation.nodeId) {
      return {
        ...root,
        surface: { ...root.surface, reference: operation.reference },
      };
    }
    throw new Error("Delta targets an unavailable Surface node");
  }
  if (operation.op === "toggleSetValue") {
    const page = requireListPage(root);
    let matched = false;
    const items = page.body.items.map((item) => {
      let itemMatched = false;
      const update = (slot: ListItemSlot | undefined): ListItemSlot | undefined => {
        if (slot === undefined || !isToggleSlot(slot) || slot.id !== operation.nodeId) return slot;
        matched = true;
        itemMatched = true;
        return { ...slot, value: operation.value };
      };
      const next = {
        ...item,
        leading: update(item.leading),
        trailing: update(item.trailing),
        accessory: update(item.accessory),
      };
      return itemMatched ? { ...next, done: operation.value } : next;
    });
    if (!matched) throw new Error("Delta targets an unavailable Toggle");
    return { ...page, body: { ...page.body, items } };
  }
  if (operation.op === "checkmarkSetValue") {
    const page = requireListPage(root);
    let matched = false;
    const items = page.body.items.map((item) => {
      const update = (slot: ListItemSlot | undefined): ListItemSlot | undefined => {
        if (slot === undefined || !isCheckmarkSlot(slot) || slot.id !== operation.nodeId) {
          return slot;
        }
        matched = true;
        return { ...slot, value: operation.value };
      };
      return {
        ...item,
        leading: update(item.leading),
        trailing: update(item.trailing),
        accessory: update(item.accessory),
      };
    });
    if (!matched) throw new Error("Delta targets an unavailable Checkmark");
    return { ...page, body: { ...page.body, items } };
  }
  if (operation.op === "sparklineSetData") {
    const replace = (existing: SparklineSpec): SparklineSpec => ({
      type: "sparkline",
      id: operation.nodeId,
      series: operation.series,
      accessibilityText: operation.accessibilityText,
      ...(operation.min === null ? {} : { min: operation.min }),
      ...(operation.max === null ? {} : { max: operation.max }),
      ...(operation.caption === null ? {} : { caption: operation.caption }),
      ...(operation.unit === null ? {} : { unit: operation.unit }),
      ...(existing.activate === undefined ? {} : { activate: existing.activate }),
    });
    if (isPageNode(root) && isSparklineBodySpec(root.body)
      && root.body.id === operation.nodeId) {
      return { ...root, body: replace(root.body) };
    }
    const page = requireListPage(root);
    let matched = false;
    const items = page.body.items.map((item) => {
      if (item.trailing === undefined || !isSparklineSlot(item.trailing)
        || item.trailing.id !== operation.nodeId) return item;
      matched = true;
      return { ...item, trailing: replace(item.trailing) };
    });
    if (!matched) throw new Error("Delta targets an unavailable Sparkline");
    return { ...page, body: { ...page.body, items } };
  }
  if (operation.op === "barChartSetData") {
    if (!isPageNode(root) || !isBarChartSpec(root.body)
      || root.body.id !== operation.nodeId) {
      throw new Error("Delta targets an unavailable BarChart");
    }
    const body: BarChartSpec = {
      type: "barChart",
      id: operation.nodeId,
      bars: operation.bars,
      accessibilityText: operation.accessibilityText,
      ...(root.body.activate === undefined ? {} : { activate: root.body.activate }),
    };
    return { ...root, body };
  }
  if (operation.op === "lineChartSetData") {
    if (!isPageNode(root) || !isLineChartSpec(root.body)
      || root.body.id !== operation.nodeId) {
      throw new Error("Delta targets an unavailable LineChart");
    }
    const body: LineChartSpec = {
      type: "lineChart",
      id: operation.nodeId,
      series: operation.series,
      xAxis: operation.xAxis,
      yAxis: operation.yAxis,
      accessibilityText: operation.accessibilityText,
      ...(root.body.activate === undefined ? {} : { activate: root.body.activate }),
    };
    return { ...root, body };
  }
  if (operation.op === "gaugeSetData") {
    const replace = (existing: GaugeSpec): GaugeSpec => ({
      type: "gauge",
      id: operation.nodeId,
      ratio: operation.ratio,
      label: operation.label,
      ...(operation.caption === null || operation.caption === undefined
        ? {}
        : { caption: operation.caption }),
      accessibilityText: operation.accessibilityText,
      ...(existing.activate === undefined ? {} : { activate: existing.activate }),
    });
    if (isPageNode(root) && isGaugeSpec(root.body)
      && root.body.id === operation.nodeId) {
      return { ...root, body: replace(root.body) };
    }
    const page = requireListPage(root);
    let matched = false;
    const items = page.body.items.map((item) => {
      if (item.trailing === undefined || !isGaugeSlot(item.trailing)
        || item.trailing.id !== operation.nodeId) return item;
      matched = true;
      return { ...item, trailing: replace(item.trailing) };
    });
    if (!matched) throw new Error("Delta targets an unavailable Gauge");
    return {
      ...page,
      body: { ...page.body, items },
    };
  }
  if (operation.op === "footerSetActions") {
    if (root.id !== operation.nodeId || !isValidFooterActions({ actions: operation.actions })) {
      throw new Error("Delta targets an unavailable or invalid FooterActions root");
    }
    if (!isPageNode(root) && !isTreeNode(root) && !isMarkdownEditorNode(root)) {
      throw new Error("Delta root has no FooterActions slot");
    }
    return { ...root, footer: { actions: operation.actions } };
  }
  if (operation.op === "inputSetValue") {
    const page = requireRenderablePage(root);
    if (page.header === undefined || page.header.id !== operation.nodeId) {
      throw new Error("Delta targets an unavailable Input");
    }
    return { ...page, header: { ...page.header, value: operation.value } };
  }
  if (operation.op === "listInsertItem") {
    const page = requireListPage(root);
    if (page.body.id !== operation.listId
      || operation.index < 0
      || operation.index > page.body.items.length) {
      throw new Error("Delta targets an unavailable List insertion");
    }
    const items = page.body.items.slice();
    items.splice(operation.index, 0, operation.item);
    return { ...page, body: { ...page.body, items } };
  }
  if (operation.op === "listRemoveItem") {
    const page = requireListPage(root);
    if (page.body.id !== operation.listId) {
      throw new Error("Delta targets an unavailable List");
    }
    const items = page.body.items.filter((item) => item.id !== operation.itemId);
    if (items.length === page.body.items.length) {
      throw new Error("Delta targets an unavailable ListItem");
    }
    return { ...page, body: { ...page.body, items } };
  }
  if (operation.op === "listSetSelection") {
    const page = requireListPage(root);
    if (page.body.id !== operation.listId
      || (operation.selectedId !== null
        && !page.body.items.some((item) => item.id === operation.selectedId))) {
      throw new Error("Delta targets an unavailable List selection");
    }
    return {
      ...page,
      body: { ...page.body, selectedId: operation.selectedId ?? undefined },
    };
  }
  if (operation.op === "contentSetSelection") {
    const page = requireContentPage(root);
    if (page.body.id !== operation.contentId
      || (operation.selection !== null
        && (!page.body.lines.some((line) => line.id === operation.selection!.anchorId)
          || !page.body.lines.some((line) => line.id === operation.selection!.headId)))) {
      throw new Error("Delta targets an unavailable Content selection");
    }
    return {
      ...page,
      body: { ...page.body, selection: operation.selection ?? undefined },
    };
  }
  if (operation.op === "contentSpliceLines") {
    const page = requireContentPage(root);
    if (page.body.id !== operation.contentId || operation.index < 0
      || operation.deleteCount < 0 || operation.index > page.body.lines.length
      || operation.deleteCount > page.body.lines.length - operation.index) {
      throw new Error("Content splice is outside its collection");
    }
    const lines = page.body.lines.slice();
    lines.splice(operation.index, operation.deleteCount, ...operation.lines);
    return { ...page, body: { ...page.body, lines } };
  }
  if (operation.op === "menuSetSelection") {
    if (!isMenuNode(root) || root.id !== operation.nodeId) {
      throw new Error("Delta targets an unavailable Menu node");
    }
    const selected = operation.selectedId === null
      ? undefined
      : root.items.find((item) => item.id === operation.selectedId);
    if (operation.selectedId !== null && (selected === undefined || selected.disabled === true)) {
      throw new Error("Delta selects an unavailable Menu item");
    }
    return { ...root, selectedId: operation.selectedId ?? undefined };
  }
  if (operation.op === "treeSetFilter") {
    if (!isTreeNode(root) || root.filter?.id !== operation.filterId) {
      throw new Error("Delta targets an unavailable Tree filter");
    }
    return { ...root, filter: { ...root.filter, value: operation.value } };
  }
  if (operation.op === "treeSetSelection"
    || operation.op === "treeSetLocation"
    || operation.op === "treeSpliceChildren"
    || operation.op === "treeSetChildState"
    || operation.op === "treeSetExpanded") {
    if (!isTreeNode(root) || ("nodeId" in operation && root.id !== operation.nodeId)) {
      throw new Error("Delta targets an unavailable Tree node");
    }
    switch (operation.op) {
      case "treeSetSelection":
        if (operation.selectedId !== null
          && !flattenTreeItems(root.items).some((item) => item.id === operation.selectedId)) {
          throw new Error("Delta selects an unavailable Tree item");
        }
        return { ...root, selectedId: operation.selectedId ?? undefined };
      case "treeSetLocation":
        return { ...root, location: operation.location };
      case "treeSpliceChildren": {
        let items: TreeItem[];
        if (operation.parentId === undefined) {
          if (operation.index < 0 || operation.deleteCount < 0
            || operation.index > root.items.length
            || operation.deleteCount > root.items.length - operation.index) {
            throw new Error("Tree root splice is outside its collection");
          }
          items = root.items.slice();
          items.splice(operation.index, operation.deleteCount, ...operation.items);
        } else {
          const result = updateTreeItem(root.items, operation.parentId, (parent) => {
            if (parent.kind !== "directory") return undefined;
            const children = (parent.children ?? []).slice();
            if (operation.index < 0 || operation.deleteCount < 0
              || operation.index > children.length
              || operation.deleteCount > children.length - operation.index) return undefined;
            children.splice(operation.index, operation.deleteCount, ...operation.items);
            return { ...parent, children };
          });
          if (!result.found || result.invalid) {
            throw new Error("Delta targets unavailable Tree children");
          }
          items = result.items;
        }
        const next = { ...root, items };
        if (!isValidTree(next)) throw new Error("Tree splice produced an invalid hierarchy");
        return next;
      }
      case "treeSetChildState": {
        const result = updateTreeItem(root.items, operation.itemId, (item) => (
          operation.childState === "loaded"
            ? { ...item, childState: operation.childState }
            : { ...item, childState: operation.childState, children: [] }
        ));
        if (!result.found) throw new Error("Delta targets an unavailable Tree item");
        return { ...root, items: result.items };
      }
      case "treeSetExpanded": {
        const result = updateTreeItem(root.items, operation.itemId, (item) => (
          item.kind === "directory" ? { ...item, expanded: operation.expanded } : undefined
        ));
        if (!result.found || result.invalid) {
          throw new Error("Delta targets an unavailable expandable Tree item");
        }
        return { ...root, items: result.items };
      }
    }
  }
  if (root.id !== operation.nodeId || !isMarkdownEditorNode(root)) {
    throw new Error("Delta targets an unavailable Markdown node");
  }
  switch (operation.op) {
    case "markdownReplaceRange": {
      const start = utf16PositionOffset(root.text, operation.edit.range.start);
      const end = utf16PositionOffset(root.text, operation.edit.range.end);
      if (start > end) throw new Error("Markdown text edit range is reversed");
      return {
        ...root,
        text: root.text.slice(0, start) + operation.edit.text + root.text.slice(end),
      };
    }
    case "markdownSetSelection":
      return { ...root, selection: operation.selection };
    case "markdownSetPresentation":
      return { ...root, presentation: operation.presentation };
    case "markdownSetDirty":
      return { ...root, dirty: operation.dirty };
    case "markdownSetReadOnly":
      return { ...root, readOnly: operation.readOnly };
    case "markdownSetTitle": {
      const { title: _oldTitle, ...withoutTitle } = root;
      return operation.title === null
        ? withoutTitle
        : { ...withoutTitle, title: operation.title };
    }
    case "markdownSetPlaceholder":
      return { ...root, placeholder: operation.placeholder };
    case "markdownSetCommandHint": {
      const { commandHint: _oldHint, ...withoutHint } = root;
      return operation.commandHint === null
        ? withoutHint
        : { ...withoutHint, commandHint: operation.commandHint };
    }
    case "markdownSetActions":
      return { ...root, actions: operation.actions };
    case "markdownSetMenus":
      return {
        ...root,
        insertMenu: operation.insertMenu,
        contextMenu: operation.contextMenu,
      };
    default: {
      const unreachable: never = operation;
      throw new Error(`Unsupported delta operation ${String(unreachable)}`);
    }
  }
}

function updateTreeItem(
  items: readonly TreeItem[],
  id: string,
  update: (item: TreeItem) => TreeItem | undefined,
): { items: TreeItem[]; found: boolean; invalid: boolean } {
  let found = false;
  let invalid = false;
  const next = items.map((item) => {
    if (item.id === id) {
      found = true;
      const updated = update(item);
      if (updated === undefined) {
        invalid = true;
        return item;
      }
      return updated;
    }
    const children = item.children ?? [];
    if (children.length === 0) return item;
    const nested = updateTreeItem(children, id, update);
    if (nested.found) {
      found = true;
      invalid ||= nested.invalid;
      return { ...item, children: nested.items };
    }
    return item;
  });
  return { items: next, found, invalid };
}

function requireRenderablePage(root: UiNode): PageNode & {
  header?: InputSpec;
  body: ListSpec | ContentSpec | SparklineSpec | BarChartSpec | LineChartSpec | GaugeSpec;
} {
  if (!isRenderablePageNode(root) && !isRenderableContentPageNode(root)
    && !isRenderableChartPageNode(root)) {
    throw new Error("Delta targets an unavailable Page");
  }
  return root;
}

function requireListPage(root: UiNode): PageNode & { header?: InputSpec; body: ListSpec } {
  const page = requireRenderablePage(root);
  if (!isListSpec(page.body)) throw new Error("Delta targets an unavailable List Page");
  return page as PageNode & { header?: InputSpec; body: ListSpec };
}

function requireContentPage(root: UiNode): PageNode & { header?: InputSpec; body: ContentSpec } {
  if (!isPageNode(root) || !isContentSpec(root.body)) {
    throw new Error("Delta targets an unavailable Content Page");
  }
  return root as PageNode & { header?: InputSpec; body: ContentSpec };
}

function utf16PositionOffset(text: string, position: TextPosition): number {
  const lines = text.split("\n");
  const line = lines[position.line];
  if (line === undefined || position.utf16Column < 0 || position.utf16Column > line.length) {
    throw new Error("Markdown text position is outside the document");
  }
  validatePositionInText(text, position, "delta.textPosition");
  let offset = position.utf16Column;
  for (let index = 0; index < position.line; index += 1) {
    offset += lines[index]!.length + 1;
  }
  return offset;
}

export function uiAction(
  nodeId: string,
  action: string,
  kind: UiEventKind,
  value: UiEventValue = { type: "none" },
): UiAction {
  return { nodeId, action, kind, value };
}

export function uiEvent(
  snapshot: UiSnapshot,
  participantId: string,
  rendererId: string,
  action: UiAction,
  eventId = newEventId(),
): UiEvent {
  return {
    type: "event",
    protocol: UI_PROTOCOL_NAME,
    protocolVersion: snapshot.protocolVersion,
    appInstanceId: snapshot.appInstanceId,
    participantId,
    clientId: snapshot.clientId,
    rendererId,
    viewId: snapshot.viewId,
    eventId,
    baseRevision: snapshot.revision,
    ...action,
  };
}

export function newEventId(): string {
  if (typeof crypto !== "undefined" && typeof crypto.randomUUID === "function") {
    return crypto.randomUUID();
  }
  return `event-${Date.now().toString(16)}-${Math.random().toString(16).slice(2)}`;
}

function validateAction(value: Record<string, unknown>, path: string): void {
  requireIdentifier(value.nodeId, `${path}.nodeId`);
  requireIdentifier(value.action, `${path}.action`);
  if (!["activate", "select", "change", "submit", "cancel", "command"].includes(String(value.kind))) {
    throw new Error(`Unsupported event kind ${String(value.kind)}`);
  }
  validateEventValue(value.value, `${path}.value`);
}

function validateRoute(
  value: Record<string, unknown>,
  path: string,
  includeRenderer: boolean,
): void {
  requireIdentifier(value.appInstanceId, `${path}.appInstanceId`);
  requireIdentifier(value.clientId, `${path}.clientId`);
  if (includeRenderer) requireIdentifier(value.rendererId, `${path}.rendererId`);
  requireIdentifier(value.viewId, `${path}.viewId`);
}

function validateParticipant(value: unknown, path: string): void {
  const participant = record(value, path);
  requireIdentifier(participant.id, `${path}.id`);
  if (participant.kind !== undefined
    && !["human", "agent", "service"].includes(String(participant.kind))) {
    throw new Error(`${path}.kind is unsupported`);
  }
  if (participant.sourceSessionId !== undefined) {
    requireIdentifier(participant.sourceSessionId, `${path}.sourceSessionId`);
  }
  if (participant.displayName !== undefined) {
    requireString(participant.displayName, `${path}.displayName`);
  }
  if (participant.color !== undefined) {
    requireString(participant.color, `${path}.color`, true);
  }
  if (participant.grants !== undefined) {
    if (!Array.isArray(participant.grants)) {
      throw new Error(`${path}.grants must be an array`);
    }
    for (const [index, grant] of participant.grants.entries()) {
      if (grant !== "*") requireIdentifier(grant, `${path}.grants[${index}]`);
    }
  }
}

function validateNode(value: unknown, path: string): void {
  const root = record(value, path);
  requireIdentifier(root.id, `${path}.id`);
  requireIdentifier(root.type, `${path}.type`);
  if (root.type === "canvasPage") {
    validateCanvasPageNode(root, path);
    return;
  }
  if (root.type === "media") {
    validateMediaNode(root, path);
    return;
  }
  if (root.type === "menu") {
    validateMenuSpec(root, path);
    return;
  }
  if (root.type === "page") {
    validatePageNode(root, path);
    return;
  }
  if (root.type === "surface") {
    validateSurfaceNode(root, path);
    return;
  }
  if (root.type === "tree") {
    validateTreeNode(root, path);
    return;
  }
  if (root.type === "textBox") {
    validateTextBoxNode(root, path);
    return;
  }
  if (root.type !== "markdownEditor") {
    return;
  }
  requireString(root.text, `${path}.text`, true);
  validateSelection(root.selection, `${path}.selection`);
  validateSelectionInText(root.text, root.selection, `${path}.selection`);
  if (root.presentation !== undefined
    && !["source", "preview", "split"].includes(String(root.presentation))) {
    throw new Error(`Unsupported Markdown presentation ${String(root.presentation)}`);
  }
  for (const field of ["readOnly", "dirty"] as const) {
    if (root[field] !== undefined && typeof root[field] !== "boolean") {
      throw new Error(`${path}.${field} must be a boolean`);
    }
  }
  for (const field of ["placeholder", "title"] as const) {
    if (root[field] !== undefined) requireString(root[field], `${path}.${field}`, true);
  }
  if (root.actions !== undefined) validateMarkdownActions(root.actions, `${path}.actions`);
  if (root.commandHint !== undefined) {
    validateMarkdownCommandHint(root.commandHint, `${path}.commandHint`);
    const actions = root.actions === undefined ? undefined : record(root.actions, `${path}.actions`);
    if (actions?.openMenu === undefined) {
      throw new Error(`${path}.commandHint requires actions.openMenu`);
    }
  }
  if (root.insertMenu !== undefined) validateMenuSpec(root.insertMenu, `${path}.insertMenu`);
  if (root.contextMenu !== undefined) validateMenuSpec(root.contextMenu, `${path}.contextMenu`);
  if (root.footer !== undefined) validateFooterActions(root.footer, `${path}.footer`);
}

function validateFooterActions(
  value: unknown,
  path: string,
  register: (value: unknown, valuePath: string) => void = requireIdentifier,
): void {
  const footer = record(value, path);
  if (!Array.isArray(footer.actions) || footer.actions.length > 100_000) {
    throw new Error(`${path}.actions must contain at most 100000 entries`);
  }
  const ids = new Set<string>();
  const accelerators = new Set<string>();
  for (const [index, value] of footer.actions.entries()) {
    const actionPath = `${path}.actions[${index}]`;
    const action = record(value, actionPath);
    register(action.id, `${actionPath}.id`);
    if (ids.has(action.id as string)) throw new Error(`${actionPath}.id is duplicated`);
    ids.add(action.id as string);
    requireString(action.label, `${actionPath}.label`, true);
    requireSingleLine(action.label, `${actionPath}.label`);
    if (new TextEncoder().encode(action.label as string).length > 4_096) {
      throw new Error(`${actionPath}.label must contain at most 4096 UTF-8 bytes`);
    }
    requireIdentifier(action.action, `${actionPath}.action`);
    if (action.accelerator !== undefined) {
      requireString(action.accelerator, `${actionPath}.accelerator`);
      if (!isFooterAccelerator(action.accelerator as string)) {
        throw new Error(`${actionPath}.accelerator is unsupported`);
      }
      if (accelerators.has(action.accelerator as string)) {
        throw new Error(`${actionPath}.accelerator is duplicated`);
      }
      accelerators.add(action.accelerator as string);
    }
    if (action.role !== undefined && !["default", "danger"].includes(String(action.role))) {
      throw new Error(`${actionPath}.role is unsupported`);
    }
    if (action.disabled !== undefined && typeof action.disabled !== "boolean") {
      throw new Error(`${actionPath}.disabled must be boolean`);
    }
  }
}

function validateMenuSpec(value: unknown, path: string): void {
  const menu = record(value, path);
  requireString(menu.label, `${path}.label`, true);
  requireSingleLine(menu.label, `${path}.label`);
  if (menu.presentation !== undefined
    && !["popup", "context"].includes(String(menu.presentation))) {
    throw new Error(`${path}.presentation is unsupported`);
  }
  if (menu.anchor !== undefined && !["control", "caret", "pointer"].includes(String(menu.anchor))) {
    throw new Error(`${path}.anchor is unsupported`);
  }
  if (!Array.isArray(menu.items) || menu.items.length > 256) {
    throw new Error(`${path}.items must contain at most 256 entries`);
  }
  const ids = new Set<string>();
  for (const [index, value] of menu.items.entries()) {
    const itemPath = `${path}.items[${index}]`;
    const item = record(value, itemPath);
    requireIdentifier(item.id, `${itemPath}.id`);
    if (ids.has(item.id as string)) throw new Error(`${itemPath}.id is duplicated`);
    ids.add(item.id as string);
    requireString(item.label, `${itemPath}.label`, true);
    requireSingleLine(item.label, `${itemPath}.label`);
    requireIdentifier(item.action, `${itemPath}.action`);
    if (item.hint !== undefined) {
      requireString(item.hint, `${itemPath}.hint`, true);
      requireSingleLine(item.hint, `${itemPath}.hint`);
    }
    if (item.disabled !== undefined && typeof item.disabled !== "boolean") {
      throw new Error(`${itemPath}.disabled must be boolean`);
    }
    if (item.role !== undefined && !["default", "danger"].includes(String(item.role))) {
      throw new Error(`${itemPath}.role is unsupported`);
    }
  }
  if (menu.selectedId !== undefined) {
    requireIdentifier(menu.selectedId, `${path}.selectedId`);
    const selected = menu.items
      .map((item) => record(item, `${path}.items`))
      .find((item) => item.id === menu.selectedId);
    if (selected === undefined || selected.disabled === true) {
      throw new Error(`${path}.selectedId must identify an enabled item`);
    }
  }
  if (menu.dismiss !== undefined) requireIdentifier(menu.dismiss, `${path}.dismiss`);
}

function validateTreeNode(root: Record<string, unknown>, path: string): void {
  requireString(root.label, `${path}.label`, true);
  requireString(root.location, `${path}.location`, true);
  if (root.presentation !== undefined
    && !["drillDown", "outline"].includes(String(root.presentation))) {
    throw new Error(`${path}.presentation is unsupported`);
  }
  const actions = record(root.actions, `${path}.actions`);
  for (const field of ["select", "open", "parent"] as const) {
    requireIdentifier(actions[field], `${path}.actions.${field}`);
  }
  if (actions.setExpanded !== undefined) {
    requireIdentifier(actions.setExpanded, `${path}.actions.setExpanded`);
  }
  if (root.presentation === "outline" && actions.setExpanded === undefined) {
    throw new Error(`${path}.actions.setExpanded is required for outline Trees`);
  }
  if (root.filter !== undefined) {
    const filter = record(root.filter, `${path}.filter`);
    requireIdentifier(filter.id, `${path}.filter.id`);
    requireString(filter.label, `${path}.filter.label`, true);
    if (filter.value !== undefined) requireString(filter.value, `${path}.filter.value`, true);
    if (filter.placeholder !== undefined) {
      requireString(filter.placeholder, `${path}.filter.placeholder`, true);
    }
    requireIdentifier(filter.setValue, `${path}.filter.setValue`);
  }
  if (!Array.isArray(root.items) || root.items.length > 100_000) {
    throw new Error(`${path}.items must contain at most 100000 entries`);
  }
  const ids = new Set<string>();
  let count = 0;
  let parentCount = 0;
  const visit = (items: unknown[], depth: number, itemPath: string): void => {
    if (depth > 32) throw new Error(`${itemPath} exceeds Tree depth 32`);
    for (const [index, value] of items.entries()) {
      count += 1;
      if (count > 100_000) throw new Error(`${path}.items exceeds 100000 entries`);
      const currentPath = `${itemPath}[${index}]`;
      const item = record(value, currentPath);
      requireIdentifier(item.id, `${currentPath}.id`);
      if (ids.has(item.id as string)) throw new Error(`${currentPath}.id is duplicated`);
      ids.add(item.id as string);
      requireString(item.label, `${currentPath}.label`, true);
      requireSingleLine(item.label, `${currentPath}.label`);
      if (!["parent", "directory", "file"].includes(String(item.kind))) {
        throw new Error(`${currentPath}.kind is unsupported`);
      }
      for (const field of ["hidden", "symlink", "expanded"] as const) {
        if (item[field] !== undefined && typeof item[field] !== "boolean") {
          throw new Error(`${currentPath}.${field} must be a boolean`);
        }
      }
      if (item.childState !== undefined
        && !["loaded", "unloaded", "loading"].includes(String(item.childState))) {
        throw new Error(`${currentPath}.childState is unsupported`);
      }
      const children = item.children ?? [];
      if (!Array.isArray(children)) throw new Error(`${currentPath}.children must be an array`);
      if (item.kind === "parent") {
        parentCount += 1;
        if (depth !== 0 || children.length > 0 || item.expanded === true) {
          throw new Error(`${currentPath} parent must be a root leaf`);
        }
      } else if (item.kind === "file") {
        if (children.length > 0 || item.expanded === true) {
          throw new Error(`${currentPath} file cannot own or expand children`);
        }
      } else if ((item.childState ?? "loaded") !== "loaded" && children.length > 0) {
        throw new Error(`${currentPath} unloaded/loading directory cannot contain children`);
      }
      visit(children, depth + 1, `${currentPath}.children`);
    }
  };
  visit(root.items, 0, `${path}.items`);
  if (root.selectedId !== undefined) {
    requireIdentifier(root.selectedId, `${path}.selectedId`);
    if (!ids.has(root.selectedId as string)) {
      throw new Error(`${path}.selectedId must identify one Tree entry`);
    }
  }
  if (root.footer !== undefined) {
    validateFooterActions(root.footer, `${path}.footer`, (value, valuePath) => {
      requireIdentifier(value, valuePath);
      if (ids.has(value as string)) throw new Error(`${valuePath} duplicates a component id`);
      ids.add(value as string);
    });
  }
  if (parentCount > 1) throw new Error(`${path}.items accepts at most one parent entry`);
  if (root.emptyMessage !== undefined) {
    requireString(root.emptyMessage, `${path}.emptyMessage`, true);
  }
  if (root.primaryAction !== undefined) {
    const action = record(root.primaryAction, `${path}.primaryAction`);
    requireIdentifier(action.id, `${path}.primaryAction.id`);
    requireString(action.label, `${path}.primaryAction.label`, true);
    requireIdentifier(action.action, `${path}.primaryAction.action`);
    if (action.role !== undefined
      && !["default", "primary", "destructive"].includes(String(action.role))) {
      throw new Error(`${path}.primaryAction.role is unsupported`);
    }
  }
  if (root.contextMenu !== undefined) {
    validateMenuSpec(root.contextMenu, `${path}.contextMenu`);
  }
}

function validateCanvasPageNode(root: Record<string, unknown>, path: string): void {
  requireString(root.title, `${path}.title`, true);
  if (new TextEncoder().encode(root.title as string).length > 4_096) {
    throw new Error(`${path}.title must contain at most 4096 bytes`);
  }
  const ids = new Set<string>();
  const register = (value: unknown, valuePath: string): void => {
    requireIdentifier(value, valuePath);
    if (ids.has(value as string)) throw new Error(`${valuePath} duplicates a component id`);
    ids.add(value as string);
  };
  const surface = record(root.surface, `${path}.surface`);
  register(surface.id, `${path}.surface.id`);
  validateSurfaceNode(surface, `${path}.surface`);
  if (!Array.isArray(root.controls) || root.controls.length > 32) {
    throw new Error(`${path}.controls must contain at most 32 entries`);
  }
  for (const [index, value] of root.controls.entries()) {
    const controlPath = `${path}.controls[${index}]`;
    const control = record(value, controlPath);
    requireIdentifier(control.type, `${controlPath}.type`);
    if (control.type !== "button") continue;
    register(control.id, `${controlPath}.id`);
    requireString(control.label, `${controlPath}.label`, true);
    requireIdentifier(control.action, `${controlPath}.action`);
    if (control.role !== undefined
      && !["default", "primary", "destructive"].includes(String(control.role))) {
      throw new Error(`${controlPath}.role is unsupported`);
    }
  }
}

function validatePageNode(root: Record<string, unknown>, path: string): void {
  requireString(root.title, `${path}.title`, true);
  const ids = new Set<string>();
  const register = (value: unknown, valuePath: string): void => {
    requireIdentifier(value, valuePath);
    if (ids.has(value as string)) throw new Error(`${valuePath} duplicates a component id`);
    ids.add(value as string);
  };
  if (root.footer !== undefined) {
    validateFooterActions(root.footer, `${path}.footer`, register);
  }
  if (root.header !== undefined) {
    const header = record(root.header, `${path}.header`);
    requireIdentifier(header.type, `${path}.header.type`);
    if (header.type === "input") {
      register(header.id, `${path}.header.id`);
      requireString(header.label, `${path}.header.label`, true);
      if (header.value !== undefined) requireString(header.value, `${path}.header.value`, true);
      if (header.placeholder !== undefined) {
        requireString(header.placeholder, `${path}.header.placeholder`, true);
      }
      if (header.setValue !== undefined) {
        requireIdentifier(header.setValue, `${path}.header.setValue`);
      }
      if (header.submit !== undefined) requireIdentifier(header.submit, `${path}.header.submit`);
    }
  }
  const body = record(root.body, `${path}.body`);
  requireIdentifier(body.type, `${path}.body.type`);
  if (body.type === "content") {
    validateContentSpec(body, `${path}.body`, register);
    return;
  }
  if (body.type === "sparkline") {
    validateSparkline(body, `${path}.body`, register);
    return;
  }
  if (body.type === "barChart") {
    validateBarChart(body, `${path}.body`, register);
    return;
  }
  if (body.type === "lineChart") {
    validateLineChart(body, `${path}.body`, register);
    return;
  }
  if (body.type === "gauge") {
    validateGauge(body, `${path}.body`, register);
    return;
  }
  if (body.type !== "list") return;
  register(body.id, `${path}.body.id`);
  if (body.emptyMessage !== undefined) {
    requireString(body.emptyMessage, `${path}.body.emptyMessage`, true);
  }
  if (body.selectedId !== undefined) requireIdentifier(body.selectedId, `${path}.body.selectedId`);
  if (body.select !== undefined) requireIdentifier(body.select, `${path}.body.select`);
  for (const field of ["scrollPadding", "pageOverlap"] as const) {
    if (body[field] === undefined) continue;
    requireSafeInteger(body[field], `${path}.body.${field}`);
    if ((body[field] as number) > 65_535) {
      throw new Error(`${path}.body.${field} must fit in UInt16`);
    }
  }
  if (body.pageBehavior !== undefined
    && !["selection", "scroll"].includes(String(body.pageBehavior))) {
    throw new Error(`${path}.body.pageBehavior is unsupported`);
  }
  if (body.spacePagesDown !== undefined && typeof body.spacePagesDown !== "boolean") {
    throw new Error(`${path}.body.spacePagesDown must be a boolean`);
  }
  if (body.contextMenu !== undefined) {
    validateMenuSpec(body.contextMenu, `${path}.body.contextMenu`);
  }
  if (!Array.isArray(body.items) || body.items.length > 100_000) {
    throw new Error(`${path}.body.items must contain at most 100000 rows`);
  }
  for (const [index, itemValue] of body.items.entries()) {
    validateListItem(itemValue, `${path}.body.items[${index}]`, register);
  }
  if (body.selectedId !== undefined
    && !body.items.some((value) => record(value, `${path}.body.items`).id === body.selectedId)) {
    throw new Error(`${path}.body.selectedId must identify one of its items`);
  }
}

function validateContentSpec(
  body: Record<string, unknown>,
  path: string,
  register: (value: unknown, valuePath: string) => void,
): void {
  register(body.id, `${path}.id`);
  requireString(body.label, `${path}.label`, true);
  if (body.wrap !== undefined && typeof body.wrap !== "boolean") {
    throw new Error(`${path}.wrap must be boolean`);
  }
  if (body.font !== undefined && !["body", "monospace"].includes(String(body.font))) {
    throw new Error(`${path}.font is unsupported`);
  }
  if (body.emptyMessage !== undefined) requireString(body.emptyMessage, `${path}.emptyMessage`, true);
  if (body.select !== undefined) requireIdentifier(body.select, `${path}.select`);
  if (body.contextMenu !== undefined) validateMenuSpec(body.contextMenu, `${path}.contextMenu`);
  if (!Array.isArray(body.lines) || body.lines.length > 100_000) {
    throw new Error(`${path}.lines must contain at most 100000 lines`);
  }
  const lineIDs = new Set<string>();
  for (const [index, value] of body.lines.entries()) {
    const linePath = `${path}.lines[${index}]`;
    const line = record(value, linePath);
    requireIdentifier(line.id, `${linePath}.id`);
    if (lineIDs.has(line.id as string)) throw new Error(`${linePath}.id must be unique`);
    lineIDs.add(line.id as string);
    if (line.tone !== undefined
      && !["default", "muted", "header", "added", "removed"].includes(String(line.tone))) {
      throw new Error(`${linePath}.tone is unsupported`);
    }
    if (!Array.isArray(line.runs)) throw new Error(`${linePath}.runs must be an array`);
    for (const [runIndex, runValue] of line.runs.entries()) {
      const runPath = `${linePath}.runs[${runIndex}]`;
      const run = record(runValue, runPath);
      requireString(run.text, `${runPath}.text`, true);
      requireSingleLine(run.text, `${runPath}.text`);
      if (run.tone !== undefined && ![
        "default", "muted", "accent", "info", "success", "warning", "danger",
      ].includes(String(run.tone))) throw new Error(`${runPath}.tone is unsupported`);
      if (run.emphasis !== undefined
        && !["regular", "strong", "italic"].includes(String(run.emphasis))) {
        throw new Error(`${runPath}.emphasis is unsupported`);
      }
    }
  }
  if (body.selection !== undefined) {
    const selection = record(body.selection, `${path}.selection`);
    requireIdentifier(selection.anchorId, `${path}.selection.anchorId`);
    requireIdentifier(selection.headId, `${path}.selection.headId`);
    if (!lineIDs.has(selection.anchorId as string) || !lineIDs.has(selection.headId as string)
      || body.select === undefined) throw new Error(`${path}.selection is invalid`);
  }
}

function validateListItem(
  value: unknown,
  path: string,
  register: (value: unknown, valuePath: string) => void = requireIdentifier,
): void {
  const item = record(value, path);
  register(item.id, `${path}.id`);
  requireString(item.label, `${path}.label`, true);
  requireSingleLine(item.label, `${path}.label`);
  validateListItemTextRuns(item.labelRuns, item.label, `${path}.labelRuns`);
  validateOptionalListItemTone(item.labelTone, `${path}.labelTone`);
  validateOptionalListItemTone(item.valueTone, `${path}.valueTone`);
  if (item.emphasis !== undefined && !["regular", "strong"].includes(String(item.emphasis))) {
    throw new Error(`${path}.emphasis is unsupported`);
  }
  if (item.detail !== undefined) {
    requireString(item.detail, `${path}.detail`, true);
    requireSingleLine(item.detail, `${path}.detail`);
  }
  validateListItemTextRuns(item.detailRuns, item.detail, `${path}.detailRuns`);
  if (item.value !== undefined) {
    requireString(item.value, `${path}.value`, true);
    requireSingleLine(item.value, `${path}.value`);
  }
  validateListItemTextRuns(item.valueRuns, item.value, `${path}.valueRuns`);
  if (item.valueMinWidth !== undefined) {
    requireSafeInteger(item.valueMinWidth, `${path}.valueMinWidth`);
    if ((item.valueMinWidth as number) > 65_535) {
      throw new Error(`${path}.valueMinWidth must fit in UInt16`);
    }
  }
  if (item.done !== undefined && typeof item.done !== "boolean") {
    throw new Error(`${path}.done must be a boolean`);
  }
  if (item.busy !== undefined && typeof item.busy !== "boolean") {
    throw new Error(`${path}.busy must be a boolean`);
  }
  const toggleValues: boolean[] = [];
  let checkmarkCount = 0;
  let disclosureCount = 0;
  let sparklineCount = 0;
  let gaugeCount = 0;
  if (item.delete !== undefined) requireIdentifier(item.delete, `${path}.delete`);
  if (item.activate !== undefined) requireIdentifier(item.activate, `${path}.activate`);
  if (item.actionRole !== undefined
    && !["default", "destructive"].includes(String(item.actionRole))) {
    throw new Error(`${path}.actionRole is unsupported`);
  }
  for (const name of ["leading", "trailing", "accessory"] as const) {
    if (item[name] === undefined) continue;
    const slot = record(item[name], `${path}.${name}`);
    requireIdentifier(slot.type, `${path}.${name}.type`);
    if (slot.type === "status") {
      requireString(slot.symbol, `${path}.${name}.symbol`);
      requireSingleLine(slot.symbol, `${path}.${name}.symbol`);
      requireString(slot.label, `${path}.${name}.label`, true);
      validateOptionalListItemTone(slot.tone, `${path}.${name}.tone`);
      if (slot.emphasis !== undefined
        && !["regular", "strong"].includes(String(slot.emphasis))) {
        throw new Error(`${path}.${name}.emphasis is unsupported`);
      }
      if (slot.preserveToneWhenSelected !== undefined
        && typeof slot.preserveToneWhenSelected !== "boolean") {
        throw new Error(`${path}.${name}.preserveToneWhenSelected must be a boolean`);
      }
      continue;
    }
    if (slot.type === "badge") {
      requireString(slot.text, `${path}.${name}.text`, true);
      requireSingleLine(slot.text, `${path}.${name}.text`);
      validateOptionalListItemTone(slot.tone, `${path}.${name}.tone`);
      continue;
    }
    if (slot.type === "sparkline") {
      validateSparkline(slot, `${path}.${name}`, register);
      sparklineCount += 1;
      if (name !== "trailing") {
        throw new Error(`${path}.${name} Sparkline is accepted only in the trailing slot`);
      }
      continue;
    }
    if (slot.type === "gauge") {
      validateGauge(slot, `${path}.${name}`, register);
      gaugeCount += 1;
      if (name !== "trailing") {
        throw new Error(`${path}.${name} Gauge is accepted only in the trailing slot`);
      }
      continue;
    }
    if (slot.type === "toggle") {
      register(slot.id, `${path}.${name}.id`);
      requireString(slot.label, `${path}.${name}.label`, true);
      if (typeof slot.value !== "boolean") {
        throw new Error(`${path}.${name}.value must be a boolean`);
      }
      toggleValues.push(slot.value);
      requireIdentifier(slot.setValue, `${path}.${name}.setValue`);
      continue;
    }
    if (slot.type === "checkmark") {
      register(slot.id, `${path}.${name}.id`);
      requireString(slot.label, `${path}.${name}.label`, true);
      if (typeof slot.value !== "boolean") {
        throw new Error(`${path}.${name}.value must be a boolean`);
      }
      requireIdentifier(slot.setValue, `${path}.${name}.setValue`);
      checkmarkCount += 1;
      if (name !== "accessory") {
        throw new Error(`${path}.${name} Checkmark is accepted only as an accessory`);
      }
      continue;
    }
    if (slot.type === "disclosure") {
      disclosureCount += 1;
      if (name !== "accessory") {
        throw new Error(`${path}.${name} Disclosure is accepted only as an accessory`);
      }
    }
  }
  if (toggleValues.length > 1) throw new Error(`${path} accepts at most one completion Toggle`);
  if (toggleValues.length === 1 && toggleValues[0] !== (item.done ?? false)) {
    throw new Error(`${path}.done must match its completion Toggle`);
  }
  if (checkmarkCount > 1 || disclosureCount > 1) {
    throw new Error(`${path} accepts at most one Checkmark or Disclosure`);
  }
  if (sparklineCount > 1) throw new Error(`${path} accepts at most one Sparkline`);
  if (gaugeCount > 1) throw new Error(`${path} accepts at most one Gauge`);
  if (disclosureCount > 0 && item.activate === undefined) {
    throw new Error(`${path}.activate is required by Disclosure`);
  }
  const independentRoles = Number(toggleValues.length > 0)
    + Number(checkmarkCount > 0)
    + Number(disclosureCount > 0)
    + Number([item.leading, item.trailing, item.accessory].some((value) => {
      if (value === undefined) return false;
      const slot = record(value, `${path}.slots`);
      return (slot.type === "sparkline" || slot.type === "gauge")
        && slot.activate !== undefined;
    }))
    + Number(item.activate !== undefined && disclosureCount === 0);
  if (independentRoles > 1) throw new Error(`${path} primary role is ambiguous`);
  if (item.actionRole === "destructive"
    && (item.activate === undefined || disclosureCount > 0)) {
    throw new Error(`${path}.actionRole destructive requires a plain command row`);
  }
}

function validateSparkline(
  value: unknown,
  path: string,
  register: (value: unknown, valuePath: string) => void,
): void {
  const sparkline = record(value, path);
  register(sparkline.id, `${path}.id`);
  if (!Array.isArray(sparkline.series)
    || sparkline.series.length === 0
    || sparkline.series.length > 100_000
    || !sparkline.series.every((point) => typeof point === "number" && Number.isFinite(point))) {
    throw new Error(`${path}.series must contain 1...100000 finite numbers`);
  }
  for (const name of ["min", "max"] as const) {
    if (sparkline[name] !== undefined
      && (typeof sparkline[name] !== "number" || !Number.isFinite(sparkline[name]))) {
      throw new Error(`${path}.${name} must be finite`);
    }
  }
  const minimum = sparkline.min as number | undefined;
  const maximum = sparkline.max as number | undefined;
  if (minimum !== undefined && maximum !== undefined && minimum >= maximum) {
    throw new Error(`${path}.min must be less than max`);
  }
  const series = sparkline.series as number[];
  if (minimum !== undefined && series.some((point) => point < minimum)
    || maximum !== undefined && series.some((point) => point > maximum)) {
    throw new Error(`${path} bounds must contain every series point`);
  }
  for (const name of ["caption", "unit"] as const) {
    if (sparkline[name] !== undefined) {
      requireChartLabel(sparkline[name], `${path}.${name}`, true);
    }
  }
  requireChartAccessibility(sparkline.accessibilityText, `${path}.accessibilityText`);
  if (sparkline.activate !== undefined) {
    requireIdentifier(sparkline.activate, `${path}.activate`);
  }
}

function validateBarChart(
  value: unknown,
  path: string,
  register: (value: unknown, valuePath: string) => void,
): void {
  const chart = record(value, path);
  register(chart.id, `${path}.id`);
  if (!Array.isArray(chart.bars) || chart.bars.length === 0 || chart.bars.length > 1_000) {
    throw new Error(`${path}.bars must contain 1...1000 bars`);
  }
  for (const [index, value] of chart.bars.entries()) {
    const barPath = `${path}.bars[${index}]`;
    const bar = record(value, barPath);
    requireChartLabel(bar.label, `${barPath}.label`);
    requireFiniteNumber(bar.value, `${barPath}.value`);
    if ((bar.value as number) < 0) throw new Error(`${barPath}.value must be non-negative`);
    if (bar.valueCaption !== undefined) {
      requireChartLabel(bar.valueCaption, `${barPath}.valueCaption`, true);
    }
    if (bar.emphasis !== undefined
      && !["default", "accent", "danger"].includes(String(bar.emphasis))) {
      throw new Error(`${barPath}.emphasis is unsupported`);
    }
  }
  requireChartAccessibility(chart.accessibilityText, `${path}.accessibilityText`);
  if (chart.activate !== undefined) requireIdentifier(chart.activate, `${path}.activate`);
}

function validateLineChart(
  value: unknown,
  path: string,
  register: (value: unknown, valuePath: string) => void,
): void {
  const chart = record(value, path);
  register(chart.id, `${path}.id`);
  if (!Array.isArray(chart.series) || chart.series.length === 0 || chart.series.length > 16) {
    throw new Error(`${path}.series must contain 1...16 series`);
  }
  const names = new Set<string>();
  const points: Array<{ x: number; y: number }> = [];
  for (const [seriesIndex, value] of chart.series.entries()) {
    const seriesPath = `${path}.series[${seriesIndex}]`;
    const series = record(value, seriesPath);
    requireChartLabel(series.name, `${seriesPath}.name`);
    if (names.has(series.name as string)) {
      throw new Error(`${seriesPath}.name must be unique`);
    }
    names.add(series.name as string);
    if (!Array.isArray(series.points) || series.points.length === 0) {
      throw new Error(`${seriesPath}.points must not be empty`);
    }
    for (const [pointIndex, value] of series.points.entries()) {
      const pointPath = `${seriesPath}.points[${pointIndex}]`;
      const point = record(value, pointPath);
      requireFiniteNumber(point.x, `${pointPath}.x`);
      requireFiniteNumber(point.y, `${pointPath}.y`);
      points.push({ x: point.x as number, y: point.y as number });
      if (points.length > 100_000) {
        throw new Error(`${path}.series must contain at most 100000 total points`);
      }
    }
  }
  validateLineChartAxis(chart.xAxis, `${path}.xAxis`, points.map((point) => point.x));
  validateLineChartAxis(chart.yAxis, `${path}.yAxis`, points.map((point) => point.y));
  requireChartAccessibility(chart.accessibilityText, `${path}.accessibilityText`);
  if (chart.activate !== undefined) requireIdentifier(chart.activate, `${path}.activate`);
}

function validateLineChartAxis(value: unknown, path: string, points: readonly number[]): void {
  if (value === undefined) return;
  const axis = record(value, path);
  if (axis.label !== undefined) requireChartLabel(axis.label, `${path}.label`, true);
  if (axis.bounds === undefined) return;
  const bounds = record(axis.bounds, `${path}.bounds`);
  requireFiniteNumber(bounds.min, `${path}.bounds.min`);
  requireFiniteNumber(bounds.max, `${path}.bounds.max`);
  const minimum = bounds.min as number;
  const maximum = bounds.max as number;
  if (minimum >= maximum) throw new Error(`${path}.bounds.min must be less than max`);
  if (points.some((point) => point < minimum || point > maximum)) {
    throw new Error(`${path}.bounds must contain every series point`);
  }
}

function validateGauge(
  value: unknown,
  path: string,
  register: (value: unknown, valuePath: string) => void,
): void {
  const gauge = record(value, path);
  register(gauge.id, `${path}.id`);
  requireFiniteNumber(gauge.ratio, `${path}.ratio`);
  if ((gauge.ratio as number) < 0 || (gauge.ratio as number) > 1) {
    throw new Error(`${path}.ratio must be between zero and one`);
  }
  requireChartLabel(gauge.label, `${path}.label`);
  if (gauge.caption !== undefined) requireChartLabel(gauge.caption, `${path}.caption`);
  requireChartAccessibility(gauge.accessibilityText, `${path}.accessibilityText`);
  if (gauge.activate !== undefined) requireIdentifier(gauge.activate, `${path}.activate`);
}

function requireFiniteNumber(value: unknown, path: string): asserts value is number {
  if (typeof value !== "number" || !Number.isFinite(value)) {
    throw new Error(`${path} must be a finite number`);
  }
}

function requireChartLabel(value: unknown, path: string, allowEmpty = false): void {
  requireString(value, path, allowEmpty);
  requireSingleLine(value, path);
  if (value.includes("\0") || (!allowEmpty && value.trim().length === 0)
    || new TextEncoder().encode(value).length > 4_096) {
    throw new Error(`${path} must be ${allowEmpty ? "a" : "a non-empty"} single line of at most 4096 bytes`);
  }
}

function requireChartAccessibility(value: unknown, path: string): void {
  requireString(value, path);
  if (value.includes("\0") || value.includes("\r") || value.trim().length === 0
    || new TextEncoder().encode(value).length > 16_384) {
    throw new Error(`${path} must contain 1...16384 non-whitespace bytes`);
  }
}

function validateOptionalListItemTone(value: unknown, path: string): void {
  if (value === undefined) return;
  if (!["default", "muted", "accent", "info", "success", "warning", "danger"]
    .includes(String(value))) {
    throw new Error(`${path} is unsupported`);
  }
}

function validateListItemTextRuns(value: unknown, fallback: unknown, path: string): void {
  if (value === undefined) return;
  if (!Array.isArray(value) || value.length > 256) {
    throw new Error(`${path} must contain at most 256 styled runs`);
  }
  if (value.length === 0) return;
  if (typeof fallback !== "string") {
    throw new Error(`${path} requires its plain text fallback`);
  }
  let text = "";
  for (const [index, runValue] of value.entries()) {
    const runPath = `${path}[${index}]`;
    const run = record(runValue, runPath);
    requireString(run.text, `${runPath}.text`);
    requireSingleLine(run.text, `${runPath}.text`);
    if (new TextEncoder().encode(run.text as string).length > 4_096) {
      throw new Error(`${runPath}.text must contain at most 4096 bytes`);
    }
    validateOptionalListItemTone(run.tone, `${runPath}.tone`);
    if (run.emphasis !== undefined
      && !["regular", "strong"].includes(String(run.emphasis))) {
      throw new Error(`${runPath}.emphasis is unsupported`);
    }
    text += run.text as string;
  }
  if (text !== fallback) {
    throw new Error(`${path} must concatenate exactly to its plain text fallback`);
  }
}

function requireSingleLine(value: unknown, path: string): void {
  if (typeof value !== "string" || value.includes("\n") || value.includes("\r")) {
    throw new Error(`${path} must be a single line`);
  }
}

function validateTextBoxNode(root: Record<string, unknown>, path: string): void {
  if (root.text !== undefined) requireString(root.text, `${path}.text`, true);
  for (const field of ["placeholder", "prompt"] as const) {
    if (root[field] !== undefined) requireString(root[field], `${path}.${field}`, true);
  }
  if (root.titles !== undefined) {
    if (!Array.isArray(root.titles)) throw new Error(`${path}.titles must be an array`);
    const positions = new Set<string>();
    root.titles.forEach((entry, index) => {
      const title = record(entry, `${path}.titles[${index}]`);
      requireString(title.text, `${path}.titles[${index}].text`, true);
      const position = String(title.position);
      if (!["topLeft", "topRight", "bottomLeft", "bottomRight"].includes(position)) {
        throw new Error(`${path}.titles[${index}].position is unsupported`);
      }
      if (positions.has(position)) {
        throw new Error(`${path}.titles[${index}].position may appear at most once`);
      }
      positions.add(position);
    });
  }
  if (root.hints !== undefined) {
    if (!Array.isArray(root.hints)) throw new Error(`${path}.hints must be an array`);
    root.hints.forEach((entry, index) => {
      const hint = record(entry, `${path}.hints[${index}]`);
      requireString(hint.key, `${path}.hints[${index}].key`, true);
      requireString(hint.label, `${path}.hints[${index}].label`, true);
    });
  }
  if (root.busy !== undefined) {
    const busy = record(root.busy, `${path}.busy`);
    requireString(busy.label, `${path}.busy.label`, true);
    if (busy.elapsedMs !== undefined
      && (typeof busy.elapsedMs !== "number" || !Number.isInteger(busy.elapsedMs) || busy.elapsedMs < 0)) {
      throw new Error(`${path}.busy.elapsedMs must be a non-negative integer`);
    }
    if (busy.rightMeta !== undefined) requireString(busy.rightMeta, `${path}.busy.rightMeta`, true);
  }
  if (root.submitMode !== undefined && !["enter", "never"].includes(String(root.submitMode))) {
    throw new Error(`${path}.submitMode is unsupported`);
  }
  const minRows = root.minRows ?? 3;
  const maxRows = root.maxRows ?? 10;
  if (typeof minRows !== "number" || typeof maxRows !== "number"
    || !Number.isInteger(minRows) || !Number.isInteger(maxRows) || minRows < 1 || maxRows < minRows) {
    throw new Error(`${path}.minRows must be at least 1 and at most maxRows`);
  }
  if (root.actions !== undefined) {
    const actions = record(root.actions, `${path}.actions`);
    if (actions.setText !== undefined) requireIdentifier(actions.setText, `${path}.actions.setText`);
    if (actions.submit !== undefined) requireIdentifier(actions.submit, `${path}.actions.submit`);
  }
}

function validateMediaNode(root: Record<string, unknown>, path: string): void {
  validateMediaSource(root.source, `${path}.source`);
  validateMediaPixelSize(root.intrinsic, `${path}.intrinsic`);
  if (root.cells !== undefined) {
    validateMediaOptionalSize(root.cells, `${path}.cells`, 65_535);
  }
  if (root.points !== undefined) {
    validateMediaOptionalSize(root.points, `${path}.points`, 4_294_967_295);
  }
  if (root.fit !== undefined && !["contain", "cover", "fill"].includes(String(root.fit))) {
    throw new Error(`${path}.fit is unsupported`);
  }
  requireString(root.alt, `${path}.alt`, true);
  if (new TextEncoder().encode(root.alt as string).length > 16_384) {
    throw new Error(`${path}.alt must contain at most 16384 bytes`);
  }
  if (root.activate !== undefined) requireIdentifier(root.activate, `${path}.activate`);
}

function validateSurfaceNode(root: Record<string, unknown>, path: string): void {
  validateSurfaceReference(root.reference, `${path}.reference`);
  if (root.cells !== undefined) {
    validateMediaOptionalSize(root.cells, `${path}.cells`, 65_535);
  }
  if (root.points !== undefined) {
    validateMediaOptionalSize(root.points, `${path}.points`, 4_294_967_295);
  }
  if (root.background !== undefined) {
    const background = record(root.background, `${path}.background`);
    if (background.kind === "solid") {
      requireString(background.color, `${path}.background.color`);
      if (!/^#[0-9A-Fa-f]{6}([0-9A-Fa-f]{2})?$/.test(background.color as string)) {
        throw new Error(`${path}.background.color must be #RRGGBB or #RRGGBBAA sRGBA`);
      }
    } else if (background.kind !== "transparent") {
      throw new Error(`${path}.background.kind is unsupported`);
    }
  }
  if (root.inputPolicy !== undefined
    && !["none", "pointer", "pointerAndKeyboard"].includes(String(root.inputPolicy))) {
    throw new Error(`${path}.inputPolicy is unsupported`);
  }
}

function validateSurfaceReference(value: unknown, path: string): void {
  const reference = record(value, path);
  requireIdentifier(reference.sessionId, `${path}.sessionId`);
  requireIdentifier(reference.streamId, `${path}.streamId`);
}

function validateMediaSource(value: unknown, path: string): void {
  const source = record(value, path);
  switch (source.kind) {
    case "path":
      requireString(source.path, `${path}.path`);
      if (new TextEncoder().encode(source.path as string).length > 4_096
        || (source.path as string).includes("\0")) {
        throw new Error(`${path}.path must contain 1..=4096 non-NUL bytes`);
      }
      return;
    case "inline":
      validateImageMediaType(source.mediaType, `${path}.mediaType`);
      requireString(source.base64, `${path}.base64`);
      if (decodedBase64Length(source.base64 as string) > MAX_INLINE_MEDIA_BYTES) {
        throw new Error(`${path}.base64 exceeds the 256 KiB decoded limit`);
      }
      return;
    case "blob":
      requireString(source.sha256, `${path}.sha256`);
      if (!/^[0-9a-f]{64}$/.test(source.sha256 as string)) {
        throw new Error(`${path}.sha256 must be lowercase SHA-256 hex`);
      }
      validateImageMediaType(source.mediaType, `${path}.mediaType`);
      requireSafeInteger(source.byteLength, `${path}.byteLength`);
      if (source.byteLength === 0) throw new Error(`${path}.byteLength must be positive`);
      return;
    default:
      throw new Error(`Unsupported Media source ${String(source.kind)}`);
  }
}

function validateMediaPixelSize(value: unknown, path: string): void {
  const size = record(value, path);
  for (const axis of ["w", "h"] as const) {
    requireSafeInteger(size[axis], `${path}.${axis}`);
    if (size[axis] === 0 || (size[axis] as number) > 4_294_967_295) {
      throw new Error(`${path}.${axis} must be a positive UInt32`);
    }
  }
}

function validateMediaOptionalSize(value: unknown, path: string, maximum: number): void {
  const size = record(value, path);
  if (size.w === undefined && size.h === undefined) {
    throw new Error(`${path} must contain at least one axis`);
  }
  for (const axis of ["w", "h"] as const) {
    if (size[axis] === undefined) continue;
    requireSafeInteger(size[axis], `${path}.${axis}`);
    if (size[axis] === 0 || (size[axis] as number) > maximum) {
      throw new Error(`${path}.${axis} is outside the supported range`);
    }
  }
}

function validateImageMediaType(value: unknown, path: string): void {
  requireString(value, path);
  if ((value as string).length > 127
    || !/^image\/[A-Za-z0-9!#$&^_.+/-]+$/.test(value as string)) {
    throw new Error(`${path} must be a portable image MIME type`);
  }
}

function decodedBase64Length(value: string): number {
  if (value.length === 0 || value.length > 349_528 || value.length % 4 !== 0
    || !/^[A-Za-z0-9+/]*={0,2}$/.test(value)) {
    throw new Error("Media inline base64 is invalid");
  }
  const decoded = atob(value);
  if (btoa(decoded) !== value) throw new Error("Media inline base64 is non-canonical");
  const length = decoded.length;
  if (length <= 0) throw new Error("Media inline base64 is empty");
  return length;
}

function validateMarkdownActions(value: unknown, path: string): void {
  const actions = record(value, path);
  for (const field of [
    "replaceRange",
    "setSelection",
    "save",
    "undo",
    "redo",
    "setPresentation",
    "openMenu",
  ]) {
    if (actions[field] !== undefined) requireIdentifier(actions[field], `${path}.${field}`);
  }
}

function validateMarkdownCommandHint(value: unknown, path: string): void {
  const hint = record(value, path);
  requireString(hint.text, `${path}.text`);
  const text = hint.text as string;
  if (new TextEncoder().encode(text).length > 4_096
    || text.includes("\0") || text.includes("\r") || text.includes("\n")) {
    throw new Error(`${path}.text must be a non-empty single line of at most 4096 bytes`);
  }
  if (hint.visibility !== "cursorOnEmptyLineOutsideCodeFence") {
    throw new Error(`${path}.visibility is unsupported`);
  }
}

function isValidMarkdownCommandHint(hint: MarkdownCommandHint): boolean {
  try {
    validateMarkdownCommandHint(hint, "commandHint");
    return true;
  } catch {
    return false;
  }
}

function validateDeltaOperation(value: unknown, path: string): void {
  const operation = record(value, path);
  switch (operation.op) {
    case "replaceRoot":
      validateNode(operation.root, `${path}.root`);
      return;
    case "markdownReplaceRange": {
      requireIdentifier(operation.nodeId, `${path}.nodeId`);
      const edit = record(operation.edit, `${path}.edit`);
      const range = record(edit.range, `${path}.edit.range`);
      validatePosition(range.start, `${path}.edit.range.start`);
      validatePosition(range.end, `${path}.edit.range.end`);
      requireString(edit.text, `${path}.edit.text`, true);
      const start = record(range.start, `${path}.edit.range.start`);
      const end = record(range.end, `${path}.edit.range.end`);
      if ((start.line as number) > (end.line as number)
        || (start.line === end.line
          && (start.utf16Column as number) > (end.utf16Column as number))) {
        throw new Error(`${path}.edit.range must not be reversed`);
      }
      return;
    }
    case "markdownSetSelection":
      requireIdentifier(operation.nodeId, `${path}.nodeId`);
      validateSelection(operation.selection, `${path}.selection`);
      return;
    case "markdownSetPresentation":
      requireIdentifier(operation.nodeId, `${path}.nodeId`);
      if (!["source", "preview", "split"].includes(String(operation.presentation))) {
        throw new Error(`${path}.presentation is unsupported`);
      }
      return;
    case "markdownSetDirty":
      requireIdentifier(operation.nodeId, `${path}.nodeId`);
      if (typeof operation.dirty !== "boolean") throw new Error(`${path}.dirty must be boolean`);
      return;
    case "markdownSetReadOnly":
      requireIdentifier(operation.nodeId, `${path}.nodeId`);
      if (typeof operation.readOnly !== "boolean") {
        throw new Error(`${path}.readOnly must be boolean`);
      }
      return;
    case "markdownSetTitle":
      requireIdentifier(operation.nodeId, `${path}.nodeId`);
      if (operation.title !== null) requireString(operation.title, `${path}.title`, true);
      return;
    case "markdownSetPlaceholder":
      requireIdentifier(operation.nodeId, `${path}.nodeId`);
      requireString(operation.placeholder, `${path}.placeholder`, true);
      return;
    case "markdownSetCommandHint":
      requireIdentifier(operation.nodeId, `${path}.nodeId`);
      if (operation.commandHint !== null) {
        validateMarkdownCommandHint(operation.commandHint, `${path}.commandHint`);
      }
      return;
    case "markdownSetActions":
      requireIdentifier(operation.nodeId, `${path}.nodeId`);
      validateMarkdownActions(operation.actions, `${path}.actions`);
      return;
    case "markdownSetMenus":
      requireIdentifier(operation.nodeId, `${path}.nodeId`);
      if (operation.insertMenu !== undefined) {
        validateMenuSpec(operation.insertMenu, `${path}.insertMenu`);
      }
      if (operation.contextMenu !== undefined) {
        validateMenuSpec(operation.contextMenu, `${path}.contextMenu`);
      }
      return;
    case "menuSetSelection":
      requireIdentifier(operation.nodeId, `${path}.nodeId`);
      if (operation.selectedId !== null) {
        requireIdentifier(operation.selectedId, `${path}.selectedId`);
      }
      return;
    case "mediaSetSource":
      requireIdentifier(operation.nodeId, `${path}.nodeId`);
      validateMediaSource(operation.source, `${path}.source`);
      validateMediaPixelSize(operation.intrinsic, `${path}.intrinsic`);
      return;
    case "surfaceSetReference":
      requireIdentifier(operation.nodeId, `${path}.nodeId`);
      validateSurfaceReference(operation.reference, `${path}.reference`);
      return;
    case "toggleSetValue":
    case "checkmarkSetValue":
      requireIdentifier(operation.nodeId, `${path}.nodeId`);
      if (typeof operation.value !== "boolean") throw new Error(`${path}.value must be boolean`);
      return;
    case "sparklineSetData":
      requireIdentifier(operation.nodeId, `${path}.nodeId`);
      validateSparkline({
        type: "sparkline",
        id: operation.nodeId,
        series: operation.series,
        min: operation.min ?? undefined,
        max: operation.max ?? undefined,
        caption: operation.caption ?? undefined,
        unit: operation.unit ?? undefined,
        accessibilityText: operation.accessibilityText,
      }, path, requireIdentifier);
      return;
    case "barChartSetData":
      requireIdentifier(operation.nodeId, `${path}.nodeId`);
      validateBarChart({
        type: "barChart",
        id: operation.nodeId,
        bars: operation.bars,
        accessibilityText: operation.accessibilityText,
      }, path, requireIdentifier);
      return;
    case "lineChartSetData":
      requireIdentifier(operation.nodeId, `${path}.nodeId`);
      validateLineChart({
        type: "lineChart",
        id: operation.nodeId,
        series: operation.series,
        xAxis: operation.xAxis,
        yAxis: operation.yAxis,
        accessibilityText: operation.accessibilityText,
      }, path, requireIdentifier);
      return;
    case "gaugeSetData":
      requireIdentifier(operation.nodeId, `${path}.nodeId`);
      validateGauge({
        type: "gauge",
        id: operation.nodeId,
        ratio: operation.ratio,
        label: operation.label,
        caption: operation.caption ?? undefined,
        accessibilityText: operation.accessibilityText,
      }, path, requireIdentifier);
      return;
    case "footerSetActions":
      requireIdentifier(operation.nodeId, `${path}.nodeId`);
      validateFooterActions({ actions: operation.actions }, path);
      return;
    case "inputSetValue":
      requireIdentifier(operation.nodeId, `${path}.nodeId`);
      requireString(operation.value, `${path}.value`, true);
      return;
    case "listInsertItem":
      requireIdentifier(operation.listId, `${path}.listId`);
      requireSafeInteger(operation.index, `${path}.index`);
      validateListItem(operation.item, `${path}.item`);
      return;
    case "listRemoveItem":
      requireIdentifier(operation.listId, `${path}.listId`);
      requireIdentifier(operation.itemId, `${path}.itemId`);
      return;
    case "listSetSelection":
      requireIdentifier(operation.listId, `${path}.listId`);
      if (operation.selectedId !== null) {
        requireIdentifier(operation.selectedId, `${path}.selectedId`);
      }
      return;
    case "contentSetSelection":
      requireIdentifier(operation.contentId, `${path}.contentId`);
      if (operation.selection !== null) {
        const selection = record(operation.selection, `${path}.selection`);
        requireIdentifier(selection.anchorId, `${path}.selection.anchorId`);
        requireIdentifier(selection.headId, `${path}.selection.headId`);
      }
      return;
    case "contentSpliceLines": {
      requireIdentifier(operation.contentId, `${path}.contentId`);
      requireSafeInteger(operation.index, `${path}.index`);
      requireSafeInteger(operation.deleteCount, `${path}.deleteCount`);
      const fixture = {
        type: "content",
        id: "validation-content",
        label: "Content",
        lines: operation.lines,
      };
      validateContentSpec(
        fixture,
        `${path}.lines`,
        (value, valuePath) => requireIdentifier(value, valuePath),
      );
      return;
    }
    case "treeSetSelection":
      requireIdentifier(operation.nodeId, `${path}.nodeId`);
      if (operation.selectedId !== null) {
        requireIdentifier(operation.selectedId, `${path}.selectedId`);
      }
      return;
    case "treeSetFilter":
      requireIdentifier(operation.filterId, `${path}.filterId`);
      requireString(operation.value, `${path}.value`, true);
      return;
    case "treeSetLocation":
      requireIdentifier(operation.nodeId, `${path}.nodeId`);
      requireString(operation.location, `${path}.location`, true);
      return;
    case "treeSpliceChildren":
      requireIdentifier(operation.nodeId, `${path}.nodeId`);
      if (operation.parentId !== undefined) {
        requireIdentifier(operation.parentId, `${path}.parentId`);
      }
      requireSafeInteger(operation.index, `${path}.index`);
      requireSafeInteger(operation.deleteCount, `${path}.deleteCount`);
      if (!Array.isArray(operation.items)) throw new Error(`${path}.items must be an array`);
      for (const [index, item] of operation.items.entries()) {
        const fixture = {
          id: "validation-tree",
          type: "tree",
          label: "Tree",
          location: ".",
          items: [item],
          actions: { select: "select", open: "open", parent: "parent" },
        };
        validateTreeNode(fixture, `${path}.items[${index}]`);
      }
      return;
    case "treeSetChildState":
      requireIdentifier(operation.nodeId, `${path}.nodeId`);
      requireIdentifier(operation.itemId, `${path}.itemId`);
      if (!["loaded", "unloaded", "loading"].includes(String(operation.childState))) {
        throw new Error(`${path}.childState is unsupported`);
      }
      return;
    case "treeSetExpanded":
      requireIdentifier(operation.nodeId, `${path}.nodeId`);
      requireIdentifier(operation.itemId, `${path}.itemId`);
      if (typeof operation.expanded !== "boolean") {
        throw new Error(`${path}.expanded must be a boolean`);
      }
      return;
    default:
      throw new Error(`Unsupported delta operation ${String(operation.op)}`);
  }
}

function validateRenderer(value: unknown, path: string): void {
  const renderer = record(value, path);
  requireIdentifier(renderer.id, `${path}.id`);
  requireIdentifier(renderer.kind, `${path}.kind`);
  if (renderer.capabilities !== undefined) {
    if (!Array.isArray(renderer.capabilities)) {
      throw new Error(`${path}.capabilities must be an array`);
    }
    for (const [index, capability] of renderer.capabilities.entries()) {
      requireIdentifier(capability, `${path}.capabilities[${index}]`);
    }
  }
}

function validateRendererState(value: unknown, path: string): void {
  const state = record(value, path);
  if (typeof state.rendererVisible !== "boolean" || typeof state.terminalVisible !== "boolean") {
    throw new Error(`${path} must contain boolean visibility fields`);
  }
}

function validateSelection(value: unknown, path: string): void {
  const selection = record(value, path);
  validatePosition(selection.anchor, `${path}.anchor`);
  validatePosition(selection.head, `${path}.head`);
}

function validatePosition(value: unknown, path: string): void {
  const position = record(value, path);
  requireSafeInteger(position.line, `${path}.line`);
  requireSafeInteger(position.utf16Column, `${path}.utf16Column`);
  if (position.line > 4_294_967_295 || position.utf16Column > 4_294_967_295) {
    throw new Error(`${path} exceeds the cross-renderer text position range`);
  }
}

function validateSelectionInText(text: string, value: unknown, path: string): void {
  const selection = record(value, path);
  validatePositionInText(text, selection.anchor, `${path}.anchor`);
  validatePositionInText(text, selection.head, `${path}.head`);
}

function validatePositionInText(text: string, value: unknown, path: string): void {
  const position = record(value, path);
  const line = position.line as number;
  const column = position.utf16Column as number;
  const textLine = text.split("\n")[line];
  if (textLine === undefined || column > textLine.length) {
    throw new Error(`${path} is outside the Markdown document`);
  }
  if (column > 0 && column < textLine.length) {
    const previous = textLine.charCodeAt(column - 1);
    const next = textLine.charCodeAt(column);
    if (previous >= 0xD800 && previous <= 0xDBFF
      && next >= 0xDC00 && next <= 0xDFFF) {
      throw new Error(`${path} splits a UTF-16 surrogate pair`);
    }
  }
}

function validateEventValue(value: unknown, path: string): void {
  const eventValue = record(value, path);
  switch (eventValue.type) {
    case "none":
      return;
    case "bool":
      if (typeof eventValue.value !== "boolean") {
        throw new Error(`${path}.value must be a boolean`);
      }
      return;
    case "index":
      requireSafeInteger(eventValue.value, `${path}.value`);
      return;
    case "integer":
      if (!Number.isSafeInteger(eventValue.value)) {
        throw new Error(`${path}.value must be a safe integer`);
      }
      return;
    case "number":
      if (typeof eventValue.value !== "number" || !Number.isFinite(eventValue.value)) {
        throw new Error(`${path}.value must be a finite number`);
      }
      return;
    case "text":
      requireString(eventValue.value, `${path}.value`, true);
      return;
    case "textList":
      if (!Array.isArray(eventValue.value)
        || eventValue.value.some((item) => typeof item !== "string")) {
        throw new Error(`${path}.value must be an array of strings`);
      }
      return;
    case "textEdit": {
      const edit = record(eventValue.value, `${path}.value`);
      const range = record(edit.range, `${path}.value.range`);
      validatePosition(range.start, `${path}.value.range.start`);
      validatePosition(range.end, `${path}.value.range.end`);
      requireString(edit.text, `${path}.value.text`, true);
      const start = record(range.start, `${path}.value.range.start`);
      const end = record(range.end, `${path}.value.range.end`);
      if ((start.line as number) > (end.line as number)
        || (start.line === end.line
          && (start.utf16Column as number) > (end.utf16Column as number))) {
        throw new Error(`${path}.value.range must not be reversed`);
      }
      return;
    }
    case "textSelection":
      validateSelection(eventValue.value, `${path}.value`);
      return;
    default:
      throw new Error(`Unsupported event value ${String(eventValue.type)}`);
  }
}

function record(value: unknown, path: string): Record<string, unknown> {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    throw new Error(`${path} must be an object`);
  }
  return value as Record<string, unknown>;
}

function requireSupportedProtocolVersion(value: unknown): number {
  requireSafeInteger(value, "protocolVersion");
  if (value < UI_PROTOCOL_MIN_VERSION || value > UI_PROTOCOL_MAX_VERSION) {
    throw new Error(`Unsupported UI protocol version ${String(value)}`);
  }
  return value;
}

function validateProtocolRange(minimum: unknown, maximum: unknown, path: string): void {
  requireSafeInteger(minimum, `${path}.minProtocolVersion`);
  requireSafeInteger(maximum, `${path}.maxProtocolVersion`);
  if (minimum < 1 || minimum > maximum || maximum > 4_294_967_295) {
    throw new Error(`${path} contains an invalid UI protocol version range`);
  }
}

function requireString(
  value: unknown,
  path: string,
  allowEmpty = false,
): asserts value is string {
  if (typeof value !== "string" || (!allowEmpty && value.length === 0)) {
    throw new Error(`${path} must be ${allowEmpty ? "a string" : "a non-empty string"}`);
  }
}

function requireSafeInteger(value: unknown, path: string): asserts value is number {
  if (!Number.isSafeInteger(value) || (value as number) < 0) {
    throw new Error(`${path} must be a non-negative safe integer`);
  }
}

function requireIdentifier(value: unknown, path: string): asserts value is string {
  requireString(value, path);
  if (value.length > 256 || !/^[A-Za-z0-9._:/-]+$/.test(value)) {
    throw new Error(`${path} must be a portable identifier`);
  }
}
