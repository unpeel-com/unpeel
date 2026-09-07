//! Frozen wire contract for the v1 component vocabulary and event envelope.
//!
//! This is the Ionic-direction lock: renderers (Ratatui, SwiftUI, web) and the
//! shared fixtures in `protocol/` all agree on these exact strings. Renaming a
//! kind, capability, event, or value tag breaks cross-renderer Apps silently,
//! so any change here must be a deliberate, reviewed diff that also updates
//! `protocol/unpeel-ui-v1.schema.json`, `protocol/unpeel-ui-v1.ndjson`,
//! `web/src/protocol.ts`, and the SwiftUI protocol models.
//!
//! The component match below is intentionally exhaustive with no wildcard arm:
//! adding a ninth `UiComponent` variant fails compilation until its wire name
//! and capability are pinned here too.
#![cfg(feature = "ui-bridge")]

use unpeel_app_kit::{
    CanvasPage, List, MarkdownEditorSpec, MediaPixelSize, MediaSource, MediaSpec, Page,
    SemanticMenu, SurfaceReference, SurfaceSpec, TextBoxSpec, TextEdit, TextRange, TextSelection,
    Tree, UiComponent, UiEventKind, UiEventValue,
};

fn frozen_roots() -> Vec<UiComponent> {
    let surface = || SurfaceSpec::new(SurfaceReference::new("session", "stream"));
    vec![
        UiComponent::CanvasPage(CanvasPage::new("frozen", "surface", surface())),
        UiComponent::MarkdownEditor(MarkdownEditorSpec::new("", TextSelection::default())),
        UiComponent::Media(MediaSpec::new(
            MediaSource::path("/tmp/frozen.png"),
            MediaPixelSize::new(1, 1),
            "frozen",
        )),
        UiComponent::Menu(SemanticMenu::new("frozen", Vec::new())),
        UiComponent::Page(Page::new("Frozen", List::new("list", Vec::new()))),
        UiComponent::Surface(surface()),
        UiComponent::TextBox(TextBoxSpec::default()),
        UiComponent::Tree(Tree::new("frozen", "/tmp", Vec::new())),
    ]
}

#[test]
fn component_kinds_match_their_wire_tags_and_capabilities() {
    let roots = frozen_roots();
    assert_eq!(roots.len(), 8);

    for root in &roots {
        // `kind()` must stay identical to the `type` tag Swift, web, and the
        // schema switch on when deserializing this exact value.
        let wire = serde_json::to_value(root).unwrap();
        assert_eq!(wire.get("type").and_then(|tag| tag.as_str()), Some(root.kind()));

        // Capabilities are the renderer feature flags; they are wire too.
        let (kind, capability) = match root {
            UiComponent::CanvasPage(_) => ("canvasPage", "canvasPage"),
            UiComponent::MarkdownEditor(_) => ("markdownEditor", "markdownEditor"),
            UiComponent::Media(_) => ("media", "media"),
            UiComponent::Menu(_) => ("menu", "menu"),
            UiComponent::Page(_) => ("page", "page"),
            UiComponent::Surface(_) => ("surface", "surface"),
            UiComponent::TextBox(_) => ("textBox", "textBox"),
            UiComponent::Tree(_) => ("tree", "tree"),
        };
        assert_eq!(root.kind(), kind);
        assert_eq!(root.required_capability(), capability);
    }
}

#[test]
fn event_kinds_have_frozen_camel_case_wire_names() {
    let cases = [
        (UiEventKind::Activate, "activate"),
        (UiEventKind::Select, "select"),
        (UiEventKind::Change, "change"),
        (UiEventKind::Submit, "submit"),
        (UiEventKind::Cancel, "cancel"),
        (UiEventKind::Command, "command"),
    ];
    assert_eq!(cases.len(), 6);
    for (kind, wire) in cases {
        let value = serde_json::to_value(kind).unwrap();
        assert_eq!(value.as_str(), Some(wire));
        assert_eq!(serde_json::from_value::<UiEventKind>(value).unwrap(), kind);
    }
}

#[test]
fn event_values_have_frozen_tagged_wire_shapes() {
    let cases = [
        (UiEventValue::None, "none"),
        (UiEventValue::Bool(true), "bool"),
        (UiEventValue::Index(1), "index"),
        (UiEventValue::Integer(-1), "integer"),
        (UiEventValue::Number(0.5), "number"),
        (UiEventValue::Text("go".to_owned()), "text"),
        (
            UiEventValue::TextList(vec!["a".to_owned()]),
            "textList",
        ),
        (
            UiEventValue::TextEdit(TextEdit::new(TextRange::default(), "x")),
            "textEdit",
        ),
        (
            UiEventValue::TextSelection(TextSelection::default()),
            "textSelection",
        ),
    ];
    assert_eq!(cases.len(), 9);
    for (value, tag) in cases {
        let wire = serde_json::to_value(&value).unwrap();
        assert_eq!(
            wire.get("type").and_then(|entry| entry.as_str()),
            Some(tag)
        );
        assert_eq!(
            serde_json::from_value::<UiEventValue>(wire).unwrap(),
            value
        );
    }
}
