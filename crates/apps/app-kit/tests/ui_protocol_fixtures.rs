#![cfg(feature = "ui-bridge")]

use std::io::{BufReader, Cursor};

use unpeel_app_kit::{
    ButtonRole, CanvasControl, ListItemActionRole, ListItemSlot, MarkdownMenuTrigger, MediaSource,
    PageBodySlot, RowPrimaryRole, SurfaceInputPolicy, UiComponent, UiEventKind, UiEventValue,
    UiMessage, read_ui_message,
};

const STREAM: &str = include_str!("../protocol/unpeel-ui-v1.ndjson");

#[test]
fn shared_v1_stream_decodes_and_validates_every_frame() {
    let mut reader = BufReader::new(Cursor::new(STREAM.as_bytes()));
    let mut messages = Vec::new();
    while let Some(message) = read_ui_message(&mut reader).unwrap() {
        messages.push(message);
    }

    assert_eq!(messages.len(), 53);
    let UiMessage::Attach(attach) = &messages[0] else {
        panic!("first fixture must attach an authenticated participant");
    };
    assert!(attach.participant_token.starts_with("upui1."));
    let UiMessage::Attached(attached) = &messages[1] else {
        panic!("second fixture must acknowledge attachment");
    };
    assert!(attached.resumed);
    let UiMessage::Snapshot(snapshot) = &messages[2] else {
        panic!("third fixture must be a snapshot");
    };
    let UiComponent::MarkdownEditor(editor) = &snapshot.root.element else {
        panic!("third fixture must contain a Markdown editor");
    };
    assert_eq!(editor.text, "# Hello\n🙂 world");
    assert_eq!(editor.selection.head.utf16_column, 2);

    let UiMessage::Presence(presence) = &messages[3] else {
        panic!("fourth fixture must contain collaborative presence");
    };
    assert_eq!(presence.members.len(), 2);

    let UiMessage::Event(edit) = &messages[4] else {
        panic!("fifth fixture must be an event");
    };
    assert_eq!(edit.event_id.as_str(), "event-edit-1");
    assert!(matches!(edit.action.value, UiEventValue::TextEdit(_)));
    assert!(matches!(messages[5], UiMessage::Ack(_)));
    assert!(matches!(messages[6], UiMessage::Lifecycle(_)));
    assert!(matches!(messages[7], UiMessage::RequestSnapshot(_)));

    let UiMessage::Event(selection) = &messages[8] else {
        panic!("ninth fixture must be an event");
    };
    assert!(matches!(
        selection.action.value,
        UiEventValue::TextSelection(_)
    ));
    let UiMessage::Event(save) = &messages[9] else {
        panic!("tenth fixture must be an event");
    };
    assert_eq!(save.action.action.as_str(), "save");
    assert_eq!(save.action.value, UiEventValue::None);
    let UiMessage::Delta(delta) = &messages[10] else {
        panic!("eleventh fixture must be a server delta");
    };
    assert_eq!(delta.base_revision, 7);
    assert_eq!(delta.revision, 8);
    assert_eq!(delta.operations.len(), 3);

    let UiMessage::Snapshot(media_snapshot) = &messages[11] else {
        panic!("twelfth fixture must be a Media snapshot");
    };
    let UiComponent::Media(media) = &media_snapshot.root.element else {
        panic!("twelfth fixture must contain Media");
    };
    assert_eq!(media.alt, "Tiny fixture pixel");
    assert_eq!(media.resolved_points(), (40, 40));

    let UiMessage::Delta(media_delta) = &messages[12] else {
        panic!("thirteenth fixture must be a Media reference delta");
    };
    let updated = media_snapshot.applying(media_delta).unwrap();
    let UiComponent::Media(media) = updated.root.element else {
        panic!("Media delta must preserve the component kind");
    };
    assert!(matches!(
        media.source,
        MediaSource::Blob {
            byte_length: 68,
            ..
        }
    ));

    let UiMessage::Snapshot(todo_snapshot) = &messages[13] else {
        panic!("fourteenth fixture must be the canonical Todo Page");
    };
    let UiComponent::Page(page) = &todo_snapshot.root.element else {
        panic!("Todo fixture must contain Page");
    };
    assert_eq!(page.title, "Todos");
    assert_eq!(page.list().items.len(), 3);
    assert_eq!(page.list().items[0].label, "Run the standalone TUI");
    assert_eq!(page.list().selected_id.as_deref(), Some("todo-1"));
    assert_eq!(page.list().select.as_deref(), Some("select-todo"));
    let Some(ListItemSlot::Toggle(toggle)) = &page.list().items[1].trailing else {
        panic!("Todo rows must expose their completion Toggle in a named slot");
    };
    assert!(!toggle.value);
    assert_eq!(
        page.input_spec().unwrap().submit.as_deref(),
        Some("add-todo")
    );

    let UiMessage::Delta(todo_delta) = &messages[14] else {
        panic!("fifteenth fixture must update Todo through a compact delta");
    };
    let updated = todo_snapshot.applying(todo_delta).unwrap();
    let UiComponent::Page(page) = updated.root.element else {
        panic!("Todo delta must preserve Page");
    };
    assert!(page.list().items[1].done);
    assert_eq!(page.list().selected_id.as_deref(), Some("todo-3"));

    let UiMessage::Snapshot(usage_snapshot) = &messages[15] else {
        panic!("sixteenth fixture must contain the Usage master/detail Page");
    };
    let UiComponent::Page(page) = &usage_snapshot.root.element else {
        panic!("Usage fixture must contain Page");
    };
    assert_eq!(page.back.as_deref(), Some("close-provider"));
    assert_eq!(
        page.list().items[0].detail.as_deref(),
        Some("Resets in 6d 18h")
    );
    assert_eq!(page.list().items[0].value.as_deref(), Some("3% used"));
    assert!(page.list().items[1].busy);
    let Some(ListItemSlot::Status(status)) = &page.list().items[0].leading else {
        panic!("Usage rows must expose their status in the leading slot");
    };
    assert_eq!(status.symbol, "✓");
    let Some(ListItemSlot::Badge(badge)) = &page.list().items[0].accessory else {
        panic!("Usage rows must expose their plan badge in the accessory slot");
    };
    assert_eq!(badge.text, "Pro");
    assert_eq!(
        page.list().selected_id.as_deref(),
        Some("provider-0-metric-0")
    );
    assert_eq!(
        page.list().items[1].activate.as_deref(),
        Some("refresh-usage")
    );

    let UiMessage::Snapshot(surface_snapshot) = &messages[16] else {
        panic!("seventeenth fixture must contain the planet Surface");
    };
    let UiComponent::Surface(surface) = &surface_snapshot.root.element else {
        panic!("planet fixture must contain Surface");
    };
    assert_eq!(surface.reference.session_id, "terminal-9");
    assert_eq!(surface.reference.stream_id, "planets");
    assert_eq!(surface.input_policy, SurfaceInputPolicy::PointerAndKeyboard);

    let UiMessage::Delta(surface_delta) = &messages[17] else {
        panic!("eighteenth fixture must switch the Surface reference");
    };
    let updated = surface_snapshot.applying(surface_delta).unwrap();
    let UiComponent::Surface(surface) = updated.root.element else {
        panic!("Surface reference delta must preserve the component kind");
    };
    assert_eq!(surface.reference.stream_id, "planets-detail");

    let UiMessage::Snapshot(canvas_snapshot) = &messages[18] else {
        panic!("nineteenth fixture must contain CanvasPage");
    };
    let UiComponent::CanvasPage(canvas) = &canvas_snapshot.root.element else {
        panic!("canvas fixture must contain CanvasPage");
    };
    assert_eq!(canvas.title, "Planet Canvas");
    assert_eq!(canvas.surface.id, "planet-canvas");
    assert_eq!(canvas.surface.surface.reference.stream_id, "canvas-planets");
    assert_eq!(
        canvas.required_capabilities(),
        vec!["canvasPage", "surface", "button"]
    );
    let CanvasControl::Button(select) = &canvas.controls[2];
    assert_eq!(select.role, ButtonRole::Primary);

    let UiMessage::Event(canvas_event) = &messages[19] else {
        panic!("twentieth fixture must activate a Canvas Button");
    };
    assert_eq!(canvas_event.action.node_id.as_str(), "canvas-next");
    assert_eq!(canvas_event.action.kind, UiEventKind::Activate);
    assert_eq!(canvas_event.action.value, UiEventValue::None);

    let UiMessage::Delta(canvas_delta) = &messages[20] else {
        panic!("twenty-first fixture must switch the nested Surface reference");
    };
    let updated = canvas_snapshot.applying(canvas_delta).unwrap();
    let UiComponent::CanvasPage(canvas) = updated.root.element else {
        panic!("nested Surface delta must preserve CanvasPage");
    };
    assert_eq!(
        canvas.surface.surface.reference.stream_id,
        "canvas-planets-detail"
    );

    let UiMessage::Snapshot(roles_snapshot) = &messages[21] else {
        panic!("twenty-second fixture must contain every ListItem row role");
    };
    let UiComponent::Page(page) = &roles_snapshot.root.element else {
        panic!("row-role fixture must contain Page");
    };
    assert_eq!(page.list().items[0].primary_role(), RowPrimaryRole::Toggle);
    assert_eq!(
        page.list().items[1].primary_role(),
        RowPrimaryRole::Disclosure
    );
    assert_eq!(
        page.list().items[2].primary_role(),
        RowPrimaryRole::Checkmark
    );
    assert_eq!(page.list().items[3].primary_role(), RowPrimaryRole::Command);
    assert_eq!(
        page.list().items[4].primary_role(),
        RowPrimaryRole::Destructive
    );
    assert_eq!(page.list().items[5].primary_role(), RowPrimaryRole::Static);
    assert_eq!(
        page.list().items[4].action_role,
        ListItemActionRole::Destructive
    );
    assert_eq!(
        page.required_capabilities(),
        vec![
            "page",
            "list",
            "listItem",
            "pageBack",
            "listItemMetadata",
            "listItemActivate",
            "listItemRole",
            "toggle",
            "listItemPresentation",
            "listSelection",
        ]
    );

    let UiMessage::Delta(roles_delta) = &messages[22] else {
        panic!("twenty-third fixture must update checkmark state compactly");
    };
    let updated = roles_snapshot.applying(roles_delta).unwrap();
    let UiComponent::Page(page) = updated.root.element else {
        panic!("row-role delta must preserve Page");
    };
    assert!(!page.list().items[2].primary_checkmark().unwrap().value);
    assert_eq!(page.list().selected_id.as_deref(), Some("row-destructive"));

    let UiMessage::Snapshot(tree_snapshot) = &messages[23] else {
        panic!("twenty-fourth fixture must contain the semantic Tree");
    };
    let UiComponent::Tree(tree) = &tree_snapshot.root.element else {
        panic!("Tree fixture must preserve the closed hierarchy");
    };
    assert_eq!(tree.location, "Writing");
    assert_eq!(tree.selected_id.as_deref(), Some("today"));
    assert_eq!(
        tree.required_capabilities(),
        vec![
            "tree",
            "treeHierarchy",
            "treeFilter",
            "treeParent",
            "button"
        ]
    );
    assert_eq!(
        tree.primary_action
            .as_ref()
            .map(|action| action.action.as_str()),
        Some("create-note")
    );
    assert!(!serde_json::to_string(tree).unwrap().contains("/Users/"));

    let UiMessage::Delta(tree_delta) = &messages[24] else {
        panic!("twenty-fifth fixture must contain keyed Tree deltas");
    };
    let updated = tree_snapshot.applying(tree_delta).unwrap();
    let UiComponent::Tree(tree) = updated.root.element else {
        panic!("Tree deltas must preserve the component kind");
    };
    assert_eq!(tree.location, "Writing/Projects");
    assert_eq!(
        tree.filter.as_ref().map(|filter| filter.value.as_str()),
        Some("draft")
    );
    assert_eq!(tree.selected_id.as_deref(), Some("draft"));
    assert_eq!(tree.items[1].children[0].label, "Draft.md");

    let UiMessage::Snapshot(menu_snapshot) = &messages[25] else {
        panic!("twenty-sixth fixture must contain Menu");
    };
    let UiComponent::Menu(menu) = &menu_snapshot.root.element else {
        panic!("Menu fixture must preserve its closed action vocabulary");
    };
    assert_eq!(menu.required_capabilities(), ["menu", "menuAnchor"]);
    assert!(menu.items[1].disabled);
    assert_eq!(
        menu.items[2].role,
        unpeel_app_kit::SemanticMenuItemRole::Danger
    );

    let UiMessage::Delta(menu_delta) = &messages[26] else {
        panic!("twenty-seventh fixture must select a Menu item");
    };
    let updated = menu_snapshot.applying(menu_delta).unwrap();
    let UiComponent::Menu(menu) = updated.root.element else {
        panic!("Menu delta must preserve the component kind");
    };
    assert_eq!(menu.selected_id.as_deref(), Some("delete"));

    let UiMessage::Snapshot(markdown_snapshot) = &messages[27] else {
        panic!("twenty-eighth fixture must embed semantic Markdown menus");
    };
    let UiComponent::MarkdownEditor(editor) = &markdown_snapshot.root.element else {
        panic!("nested Menu fixture must remain a MarkdownEditor");
    };
    assert_eq!(
        editor.insert_menu.as_ref().unwrap().anchor,
        unpeel_app_kit::SemanticMenuAnchor::Caret
    );
    assert_eq!(
        editor
            .actions
            .open_menu
            .as_ref()
            .map(|action| action.as_str()),
        Some("open-menu"),
        "semantic Markdown menus must have an explicit App-owned entry action"
    );
    assert_eq!(
        markdown_snapshot.root.required_capabilities(),
        vec!["markdownEditor", "menu", "menuAnchor"]
    );
    let UiMessage::Delta(markdown_menu_delta) = &messages[28] else {
        panic!("twenty-ninth fixture must update nested Markdown menus");
    };

    let UiMessage::Snapshot(content_snapshot) = &messages[29] else {
        panic!("thirtieth fixture must contain a read-only Content Page");
    };
    let UiComponent::Page(content_page) = &content_snapshot.root.element else {
        panic!("Content fixture must remain inside Page");
    };
    let content = content_page.content().expect("Page body must be Content");
    assert_eq!(content.lines.len(), 3);
    assert_eq!(content.selection.as_ref().unwrap().anchor_id, "line-1");
    assert!(content.context_menu.is_some());
    let UiMessage::Delta(content_delta) = &messages[30] else {
        panic!("thirty-first fixture must be a Content delta");
    };
    let updated = content_snapshot.applying(content_delta).unwrap();
    let UiComponent::Page(content_page) = updated.root.element else {
        panic!("Content delta must preserve Page");
    };
    let content = content_page.content().unwrap();
    assert_eq!(content.lines[2].id, "line-2-next");
    assert_eq!(content.selection.as_ref().unwrap().head_id, "line-2-next");
    let updated = markdown_snapshot.applying(markdown_menu_delta).unwrap();
    let UiComponent::MarkdownEditor(editor) = updated.root.element else {
        panic!("nested Menu delta must preserve MarkdownEditor");
    };
    assert!(editor.insert_menu.is_none());
    assert!(editor.context_menu.is_some());

    let UiMessage::Snapshot(hint_snapshot) = &messages[31] else {
        panic!("thirty-second fixture must contain the spec-owned Markdown command hint");
    };
    let UiComponent::MarkdownEditor(editor) = &hint_snapshot.root.element else {
        panic!("command-hint fixture must remain a MarkdownEditor");
    };
    assert!(editor.command_hint_visible());
    assert_eq!(
        editor.menu_trigger_for_text_input("/"),
        Some(MarkdownMenuTrigger::Slash)
    );
    assert_eq!(
        hint_snapshot.root.required_capabilities(),
        vec!["markdownEditor", "markdownCommandHint"]
    );

    let UiMessage::Event(open_slash) = &messages[32] else {
        panic!("thirty-third fixture must carry the semantic slash-menu intent");
    };
    assert_eq!(open_slash.action.action.as_str(), "open-menu");
    assert_eq!(
        open_slash.action.value,
        UiEventValue::Text("slash".to_owned())
    );

    let UiMessage::Delta(hint_delta) = &messages[33] else {
        panic!("last fixture must update the command hint through a compact delta");
    };
    let updated = hint_snapshot.applying(hint_delta).unwrap();
    let UiComponent::MarkdownEditor(editor) = updated.root.element else {
        panic!("command-hint delta must preserve MarkdownEditor");
    };
    assert_eq!(
        editor.command_hint.as_ref().map(|hint| hint.text.as_str()),
        Some("Type '/' for blocks")
    );
    assert!(!editor.command_hint_visible());

    let UiMessage::Snapshot(sparkline_snapshot) = &messages[34] else {
        panic!("last fixture must contain the shared Sparkline vector");
    };
    let UiComponent::Page(page) = &sparkline_snapshot.root.element else {
        panic!("Sparkline fixture must remain a Page");
    };
    let Some(ListItemSlot::Sparkline(sparkline)) = page.list().items[0].trailing.as_ref() else {
        panic!("Usage Trend must carry a trailing Sparkline");
    };
    assert_eq!(
        sparkline.values().collect::<Vec<_>>(),
        vec![0.0, 3.0, 1.5, 4.0]
    );
    assert_eq!(sparkline.resolved_bounds(), Some((0.0, 5.0)));
    assert_eq!(sparkline.normalized_values(), vec![0.0, 0.6, 0.3, 0.8]);
    assert_eq!(
        sparkline_snapshot.root.required_capabilities(),
        vec![
            "page",
            "list",
            "listItem",
            "listItemRole",
            "listItemPresentation",
            "sparkline"
        ]
    );
    let UiMessage::Delta(sparkline_delta) = &messages[35] else {
        panic!("last fixture must update only the keyed Sparkline data");
    };
    assert!(matches!(
        sparkline_delta.operations.as_slice(),
        [unpeel_app_kit::UiDeltaOperation::SparklineSetData { node_id, .. }]
            if node_id == "usage-trend-series"
    ));
    let updated = sparkline_snapshot.applying(sparkline_delta).unwrap();
    let UiComponent::Page(page) = updated.root.element else {
        panic!("Sparkline delta must preserve Page");
    };
    let Some(ListItemSlot::Sparkline(sparkline)) = page.list().items[0].trailing.as_ref() else {
        panic!("Sparkline delta must preserve the trailing slot");
    };
    assert_eq!(sparkline.values().collect::<Vec<_>>(), vec![1.0, 2.0, 5.0]);
    assert_eq!(sparkline.resolved_bounds(), Some((0.0, 5.0)));
    assert_eq!(sparkline.caption.as_deref(), Some("Latest trend"));
    assert_eq!(sparkline.activate.as_deref(), Some("open-trend"));

    let UiMessage::Snapshot(bar_snapshot) = &messages[36] else {
        panic!("BarChart fixture must be a snapshot");
    };
    let UiComponent::Page(page) = &bar_snapshot.root.element else {
        panic!("BarChart fixture must remain a Page");
    };
    let PageBodySlot::BarChart(chart) = &page.body else {
        panic!("Page body must carry BarChart");
    };
    assert_eq!(
        chart.normalized_values(),
        vec![12.0 / 18.0, 1.0, 7.0 / 18.0]
    );
    assert_eq!(
        bar_snapshot.root.required_capabilities(),
        vec!["page", "barChart"]
    );
    let UiMessage::Delta(bar_delta) = &messages[37] else {
        panic!("BarChart fixture must have a compact delta");
    };
    let updated = bar_snapshot.applying(bar_delta).unwrap();
    let UiComponent::Page(page) = updated.root.element else {
        panic!("BarChart delta must preserve Page");
    };
    let PageBodySlot::BarChart(chart) = page.body else {
        panic!("BarChart delta must preserve its slot");
    };
    assert_eq!(chart.bars[1].value.value(), 20.0);
    assert_eq!(chart.activate.as_deref(), Some("next-chart"));

    let UiMessage::Snapshot(line_snapshot) = &messages[38] else {
        panic!("LineChart fixture must be a snapshot");
    };
    let UiComponent::Page(page) = &line_snapshot.root.element else {
        panic!("LineChart fixture must remain a Page");
    };
    let PageBodySlot::LineChart(chart) = &page.body else {
        panic!("Page body must carry LineChart");
    };
    assert_eq!(chart.resolved_x_bounds(), (0.0, 2.0));
    assert_eq!(chart.resolved_y_bounds(), (0.0, 8.0));
    assert_eq!(chart.series[1].name, "Forecast");
    assert_eq!(
        line_snapshot.root.required_capabilities(),
        vec!["page", "lineChart"]
    );
    let UiMessage::Delta(line_delta) = &messages[39] else {
        panic!("LineChart fixture must have a compact delta");
    };
    let updated = line_snapshot.applying(line_delta).unwrap();
    let UiComponent::Page(page) = updated.root.element else {
        panic!("LineChart delta must preserve Page");
    };
    let PageBodySlot::LineChart(chart) = page.body else {
        panic!("LineChart delta must preserve its slot");
    };
    assert_eq!(chart.series[0].points[2].y.value(), 7.0);
    assert_eq!(chart.activate.as_deref(), Some("next-chart"));

    let UiMessage::Snapshot(gauge_snapshot) = &messages[40] else {
        panic!("Gauge fixture must be a snapshot");
    };
    let UiComponent::Page(page) = &gauge_snapshot.root.element else {
        panic!("Gauge fixture must remain a Page");
    };
    let PageBodySlot::Gauge(gauge) = &page.body else {
        panic!("Page body must carry Gauge");
    };
    assert_eq!(gauge.ratio.value(), 0.64);
    assert_eq!(gauge.percentage_label(), "Deployment  64%");
    assert_eq!(
        gauge_snapshot.root.required_capabilities(),
        vec!["page", "gauge"]
    );
    let UiMessage::Delta(gauge_delta) = &messages[41] else {
        panic!("Gauge fixture must have a compact delta");
    };
    let updated = gauge_snapshot.applying(gauge_delta).unwrap();
    let UiComponent::Page(page) = updated.root.element else {
        panic!("Gauge delta must preserve Page");
    };
    let PageBodySlot::Gauge(gauge) = page.body else {
        panic!("Gauge delta must preserve its slot");
    };
    assert_eq!(gauge.ratio.value(), 0.82);
    assert_eq!(gauge.activate.as_deref(), Some("next-chart"));

    let UiMessage::Snapshot(quota_snapshot) = &messages[42] else {
        panic!("List Gauge fixture must be a snapshot");
    };
    let UiComponent::Page(page) = &quota_snapshot.root.element else {
        panic!("List Gauge fixture must remain a Page");
    };
    let PageBodySlot::List(list) = &page.body else {
        panic!("List Gauge fixture must remain in a List");
    };
    let Some(ListItemSlot::Gauge(gauge)) = list.items[0].trailing.as_ref() else {
        panic!("Usage quota must carry a trailing Gauge");
    };
    assert_eq!(gauge.ratio.value(), 0.77);
    assert_eq!(gauge.value_label(), "77% left · Resets in 5d 14h");
    assert_eq!(page.footer.actions[0].accelerator.as_deref(), Some("a"));
    assert_eq!(page.footer.actions[1].label, "refresh");
    assert_eq!(
        quota_snapshot.root.required_capabilities(),
        vec![
            "page",
            "footerActions",
            "list",
            "listItem",
            "listItemPresentation",
            "gauge"
        ]
    );
    let UiMessage::Delta(quota_delta) = &messages[43] else {
        panic!("List Gauge fixture must have a compact delta");
    };
    let updated = quota_snapshot.applying(quota_delta).unwrap();
    let UiComponent::Page(page) = updated.root.element else {
        panic!("List Gauge delta must preserve Page");
    };
    let PageBodySlot::List(list) = &page.body else {
        panic!("List Gauge delta must preserve List");
    };
    let Some(ListItemSlot::Gauge(gauge)) = list.items[0].trailing.as_ref() else {
        panic!("List Gauge delta must preserve its trailing slot");
    };
    assert_eq!(gauge.ratio.value(), 0.61);
    assert_eq!(gauge.value_label(), "61% left · Resets in 4d");
    assert_eq!(page.footer.actions[1].label, "refreshing…");
    assert!(page.footer.actions[1].disabled);

    let UiMessage::Snapshot(text_box_snapshot) = &messages[44] else {
        panic!("text box fixture must start with a snapshot");
    };
    let UiComponent::TextBox(text_box) = &text_box_snapshot.root.element else {
        panic!("text box fixture must contain a textBox root");
    };
    assert_eq!(text_box.prompt, "❯ ");
    assert_eq!(
        text_box.titles[0].position,
        unpeel_app_kit::TitlePosition::BottomRight
    );
    assert_eq!(
        text_box_snapshot.root.required_capabilities(),
        vec![unpeel_app_kit::TEXT_BOX_COMPONENT_CAPABILITY]
    );
    let UiMessage::Event(set_text) = &messages[45] else {
        panic!("text box renderer must send set-text");
    };
    assert_eq!(set_text.action.kind, UiEventKind::Change);
    let mut terminal_box = unpeel_app_kit::TextBox::from_spec(text_box);
    let config = unpeel_app_kit::TextBoxConfig::new("chat-prompt");
    assert_eq!(
        terminal_box
            .handle_ui_event(100, &config, set_text)
            .unwrap(),
        Some(unpeel_app_kit::TextBoxUiEvent::TextChanged { changed: true })
    );
    assert_eq!(terminal_box.text(), "Ship it\n🙂 second line");
    let UiMessage::Delta(text_box_delta) = &messages[46] else {
        panic!("text box App must publish the replaced root");
    };
    let busy_snapshot = text_box_snapshot.applying(text_box_delta).unwrap();
    let UiComponent::TextBox(busy_box) = &busy_snapshot.root.element else {
        panic!("text box delta must preserve textBox");
    };
    assert_eq!(
        busy_box.busy.as_ref().map(|busy| busy.elapsed_ms),
        Some(8_500)
    );
    let UiMessage::Event(submit) = &messages[47] else {
        panic!("text box renderer must send submit");
    };
    assert_eq!(submit.action.kind, UiEventKind::Submit);
    assert_eq!(
        terminal_box.handle_ui_event(101, &config, submit).unwrap(),
        Some(unpeel_app_kit::TextBoxUiEvent::Submitted(
            "Ship it\n🙂 second line".to_owned()
        ))
    );
    assert_eq!(terminal_box.text(), "");

    let UiMessage::Snapshot(slash_snapshot) = &messages[48] else {
        panic!("slash round-trip must start from a Markdown snapshot");
    };
    let UiMessage::Event(slash_event) = &messages[49] else {
        panic!("slash round-trip must carry the renderer command");
    };
    assert_eq!(slash_event.action.action.as_str(), "open-menu");
    assert_eq!(slash_event.action.kind, UiEventKind::Command);
    assert_eq!(
        slash_event.action.value,
        UiEventValue::Text(MarkdownMenuTrigger::Slash.as_str().to_owned())
    );
    let UiMessage::Delta(open_menu_delta) = &messages[50] else {
        panic!("Rust reducer must publish the slash Menu as a delta");
    };
    let menu_snapshot = slash_snapshot.applying(open_menu_delta).unwrap();
    let UiComponent::MarkdownEditor(editor) = &menu_snapshot.root.element else {
        panic!("slash delta must preserve MarkdownEditor");
    };
    assert_eq!(editor.text, "/");
    let menu = editor.insert_menu.as_ref().expect("published insert Menu");
    assert_eq!(menu.anchor, unpeel_app_kit::SemanticMenuAnchor::Caret);
    assert_eq!(menu.selected_id.as_deref(), Some("block-heading-1"));

    let UiMessage::Event(selection_event) = &messages[51] else {
        panic!("renderer must return the selected Menu item");
    };
    assert_eq!(selection_event.action.node_id.as_str(), "block-heading-1");
    assert_eq!(
        selection_event.action.action.as_str(),
        "markdown-menu-select"
    );
    assert_eq!(selection_event.action.kind, UiEventKind::Activate);
    let UiMessage::Delta(selection_delta) = &messages[52] else {
        panic!("selected item must publish its resulting Markdown state");
    };
    let selected_snapshot = menu_snapshot.applying(selection_delta).unwrap();
    let UiComponent::MarkdownEditor(editor) = selected_snapshot.root.element else {
        panic!("selection delta must preserve MarkdownEditor");
    };
    assert_eq!(editor.text, "# ");
    assert!(editor.insert_menu.is_none());
}
