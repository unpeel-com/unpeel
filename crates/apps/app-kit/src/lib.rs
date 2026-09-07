//! Opinionated Ratatui components for ordinary terminal Apps.
//!
//! The component APIs require no Unpeel runtime or bridge setup and work in any
//! terminal. The default-on `ui-bridge` feature additionally lets a hosted App
//! publish semantic state to SwiftUI or web; disabling default features removes
//! all socket, authentication, persistence-envelope, and UI protocol code.
//! Markdown editing and terminal Media decoding/rendering are separate opt-in
//! features. Dynamic Surface embedding is separately default-off, so ordinary
//! Apps never pull `unpeel-surface` or wgpu.

#![deny(unsafe_code)]

mod agent;
mod app_metadata;
mod bar_chart;
mod click;
mod components;
mod content;
mod context;
mod drag;
mod drop_target;
mod editor;
mod explorer;
mod gauge;
mod host;
mod input;
mod keyboard;
mod line_chart;
mod list_navigation;
mod markdown;
#[cfg(feature = "markdown-text-area")]
mod markdown_interaction;
#[cfg(feature = "markdown-text-area")]
mod markdown_text_area;
#[cfg(any(feature = "media", feature = "ui-bridge"))]
mod media;
mod menu;
mod navigator;
mod opener;
mod path;
mod pointer;
#[cfg(feature = "ui-bridge")]
#[allow(unsafe_code)]
mod process_security;
mod runner;
mod scrollbar;
mod selectable;
mod semantic_menu;
mod sparkline;
mod spinner;
#[cfg(any(feature = "surface-embed", feature = "ui-bridge"))]
mod surface;
mod text_box;
mod theme;
mod tree;
#[cfg(feature = "ui-bridge")]
mod ui;
#[cfg(feature = "ui-bridge")]
mod ui_auth;
#[cfg(feature = "ui-bridge")]
mod ui_bridge;
#[cfg(feature = "ui-bridge")]
mod ui_state;
mod widgets;

pub use agent::{
    AgentBridge, AgentError, AgentProjectContext, clipboard_sequence, is_hosted, path_reference,
    send_reference_to_agent, send_to_agent,
};
pub use app_metadata::AppMetadata;
pub use bar_chart::{
    BAR_CHART_COMPONENT_CAPABILITY, BarChart, BarChartBar, BarChartEmphasis, BarChartWidget,
};
pub use click::{DEFAULT_DOUBLE_CLICK_INTERVAL, DoubleClickTracker};
pub use components::{
    BADGE_COMPONENT_CAPABILITY, BUTTON_COMPONENT_CAPABILITY, Badge, Button, ButtonRole, Checkmark,
    ComponentValidationError, FOOTER_ACTIONS_CAPABILITY, FooterAction, FooterActionRole,
    FooterActions, FooterActionsWidget, INPUT_COMPONENT_CAPABILITY, Input,
    LIST_COMPONENT_CAPABILITY, LIST_ITEM_ACTIVATE_CAPABILITY, LIST_ITEM_COMPONENT_CAPABILITY,
    LIST_ITEM_METADATA_CAPABILITY, LIST_ITEM_PRESENTATION_CAPABILITY, LIST_ITEM_ROLE_CAPABILITY,
    LIST_ITEM_STYLED_TEXT_CAPABILITY, LIST_SELECTION_CAPABILITY, List, ListItem,
    ListItemActionRole, ListItemBand, ListItemEmphasis, ListItemMedia, ListItemMediaSide,
    ListItemSlot, ListItemTextRun, ListItemTone, ListRowLayout, ListWidget, PAGE_BACK_CAPABILITY,
    PAGE_COMPONENT_CAPABILITY, Page, PageBodySlot, PageHeaderSlot, PageLayout, PagePointerDecision,
    PageTheme, PageWidget, STATUS_SYMBOL_COMPONENT_CAPABILITY, StatusSymbol,
    TOGGLE_COMPONENT_CAPABILITY, Toggle,
};
pub use content::{
    CONTENT_COMPONENT_CAPABILITY, CONTENT_SELECTION_CAPABILITY, Content, ContentEmphasis,
    ContentFont, ContentLine, ContentLineTone, ContentRun, ContentSelection, ContentState,
    ContentTheme, ContentTone, ContentWidget, MAX_CONTENT_LINES,
};
pub use context::{
    AppContext, AppMode, ProjectContext, UnpeelUser, WorkspaceContext, WorktreeContext,
};
pub use drag::{DRAG_MAP_FILENAME, DragRegion, DragSurface};
pub use drop_target::{
    DROP_TARGET_EVENT_FILENAME, DROP_TARGET_MAP_FILENAME, DropTargetEvent, DropTargetRegion,
    DropTargetSurface,
};
pub use editor::{EditorBridge, EditorError, open_in_editor};
pub use explorer::{
    Explorer, ExplorerEntry, ExplorerEntryDetail, ExplorerEvent, ExplorerInput, ExplorerTheme,
    ExplorerWidget,
};
pub use gauge::{GAUGE_COMPONENT_CAPABILITY, Gauge, GaugeWidget};
pub use host::AppReporter;
pub use input::{InputField, InputFieldAction, InputFieldTheme, InputFieldWidget};
pub use keyboard::KeyboardEnhancementGuard;
pub use line_chart::{
    LINE_CHART_COMPONENT_CAPABILITY, LineChart, LineChartAxis, LineChartBounds, LineChartPoint,
    LineChartSeries, LineChartWidget,
};
pub use list_navigation::{
    ListKeymap, ListNavigationAction, ListNavigationOutcome, ListPageBehavior, ListState,
    RowBoundaryBehavior, RowKeyDecision, RowMetrics, RowNavigationState, RowPointerDecision,
    RowPrimaryRole,
};
pub use markdown::{MarkdownCommandHint, MarkdownCommandHintVisibility};
#[cfg(all(feature = "markdown-text-area", feature = "ui-bridge"))]
pub use markdown_interaction::{
    MARKDOWN_INSERT_DISMISS_ACTION, MARKDOWN_INSERT_SELECT_ACTION, MarkdownInteractionEventError,
};
#[cfg(feature = "markdown-text-area")]
pub use markdown_interaction::{
    MARKDOWN_INSERT_ITEMS, MarkdownBlockKind, MarkdownEditorInteraction, MarkdownInsertItem,
    MarkdownInteractionOutcome, visible_markdown_insert_items,
};
#[cfg(feature = "markdown-text-area")]
pub use markdown_text_area::{
    MarkdownEditor, MarkdownEditorStyle, MarkdownTextArea, MarkdownTextAreaStyle,
};
#[cfg(all(feature = "markdown-text-area", feature = "ui-bridge"))]
pub use markdown_text_area::{
    MarkdownEditorConfig, MarkdownEditorEvent, MarkdownEditorEventError,
    MarkdownEditorTerminalLayout,
};
#[cfg(any(feature = "media", feature = "ui-bridge"))]
pub use media::{
    MAX_INLINE_MEDIA_BYTES, MEDIA_COMPONENT_CAPABILITY, MediaCellSize, MediaFit, MediaPixelSize,
    MediaPointSize, MediaSource, MediaSpec, MediaSpecError,
};
#[cfg(feature = "media")]
pub use media::{Media, MediaError, MediaPicker, MediaProtocolType, MediaWidget};
pub use menu::{MenuItem, MenuItemTone, MenuTheme, PopupMenu};
pub use navigator::Navigator;
pub use opener::{OpenOutcome, open_resource};
pub use path::display_path_from_root;
pub use pointer::{TerminalPointerPhase, TerminalPointerState};
pub use runner::{App, AppAction, AppValue, Flow, Reduce, Session, run_app};
pub use scrollbar::VerticalScrollbar;
pub use selectable::SelectableRow;
pub use semantic_menu::{
    MENU_ANCHOR_CAPABILITY, MENU_COMPONENT_CAPABILITY, SemanticMenu, SemanticMenuAnchor,
    SemanticMenuItem, SemanticMenuItemRole, SemanticMenuPresentation, SemanticMenuValidationError,
};
pub use sparkline::{
    ChartValue, SPARKLINE_COMPONENT_CAPABILITY, Sparkline, SparklinePoint, SparklineWidget,
};
pub use spinner::{SPINNER_FRAMES, Spinner, SpinnerWidget};
#[cfg(any(feature = "surface-embed", feature = "ui-bridge"))]
pub use surface::{
    CANVAS_PAGE_COMPONENT_CAPABILITY, CanvasControl, CanvasPage, CanvasPageLayout, CanvasPageTheme,
    CanvasPageWidget, CanvasSurface, SURFACE_COMPONENT_CAPABILITY, SurfaceBackground,
    SurfaceCellSize, SurfaceInputPolicy, SurfacePointSize, SurfaceReference, SurfaceSpec,
    SurfaceSpecError, SurfaceViewportSize,
};
#[cfg(feature = "surface-embed")]
pub use surface::{Surface, SurfaceFrame, SurfaceView};
pub use text_box::{
    BusyStatus, KeyHint, SubmitMode, TEXT_BOX_COMPONENT_CAPABILITY, TextBox, TextBoxAction,
    TextBoxActions, TextBoxBusy, TextBoxOutcome, TextBoxSpec, TextBoxTheme, TextBoxTitle,
    TextBoxWidget, TitlePosition,
};
#[cfg(feature = "ui-bridge")]
pub use text_box::{TextBoxConfig, TextBoxEventError, TextBoxUiEvent};
pub use theme::{
    APP_ACCENT_ENV, ColorScheme, KitTheme, SELECTABLE_LEFT_PADDING, ThemeMonitor, hosted_accent,
    hosted_accent_for_scheme,
};
pub use tree::{
    TREE_COMPONENT_CAPABILITY, TREE_FILTER_CAPABILITY, TREE_HIERARCHY_CAPABILITY,
    TREE_PARENT_CAPABILITY, Tree, TreeActions, TreeChildState, TreeFilter, TreeItem, TreeItemKind,
    TreePointerOutcome, TreePresentation, TreeState, TreeTheme, TreeValidationError, TreeWidget,
};
#[cfg(feature = "ui-bridge")]
pub use ui::{
    ActionId, AppInstanceId, ClientId, EventId, MAX_SAFE_UI_INTEGER, MAX_UI_FRAME_BYTES,
    MarkdownEditorActions, MarkdownEditorSpec, MarkdownMenuTrigger, MarkdownPresentation, NodeId,
    ParticipantId, RendererId, TextEdit, TextPosition, TextRange, TextSelection,
    UI_DELTA_CAPABILITY, UI_MARKDOWN_COMMAND_HINT_CAPABILITY, UI_MARKDOWN_EDITOR_CAPABILITY,
    UI_PROTOCOL_MAX_VERSION, UI_PROTOCOL_MIN_VERSION, UI_PROTOCOL_NAME, UI_PROTOCOL_VERSION,
    UI_SOCKET_ENV, UI_TOKEN_ENV, UiAck, UiAckStatus, UiAction, UiAttach, UiAttached, UiComponent,
    UiDelta, UiDeltaOperation, UiErrorMessage, UiEvent, UiEventKind, UiEventValue, UiGrant,
    UiLifecycle, UiMessage, UiNode, UiParticipant, UiParticipantKind, UiPresence, UiPresenceMember,
    UiProtocolError, UiRendererMetadata, UiRendererState, UiRequestSnapshot, UiSnapshot,
    UiValidationError, ViewId, decode_ui_frame, encode_ui_frame, markdown_delta_operations,
    negotiate_ui_protocol_version, page_delta_operations, read_ui_message, tree_delta_operations,
    write_ui_message,
};
#[cfg(feature = "ui-bridge")]
pub use ui_auth::{
    UI_PARTICIPANT_TOKEN_PREFIX, UI_PARTICIPANT_TOKEN_VERSION, UiParticipantTokenClaims,
    UiParticipantTokenError, UiParticipantTokenIssuer, UiParticipantTokenVerifier,
};
#[cfg(feature = "ui-bridge")]
pub use ui_bridge::{UiBridge, UiBridgeError, UiBridgeEvent, UiEventOutcome};
#[cfg(feature = "ui-bridge")]
pub use ui_state::{
    UI_STATE_FILENAME, UI_STATE_FORMAT, UI_STATE_FORMAT_VERSION, UiSavedState, UiStateError,
    UiStateStore,
};
/// Upstream scene/runtime API used by the optional Surface embed. App Kit does
/// not reinterpret USRF messages or own the GPU presenter.
#[cfg(feature = "surface-embed")]
pub use unpeel_surface as surface_runtime;
pub use widgets::{DragSource, DraggablePath};
