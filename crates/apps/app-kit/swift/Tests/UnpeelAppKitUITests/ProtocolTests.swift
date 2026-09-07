import Foundation
import Testing

@testable import UnpeelAppKitUI

@Test
func listNavigationUsesOneRoleAwareDecisionTable() {
    #expect(uiListNavigationDecision(key: .enter, primaryRole: .disclosure) == .invokePrimary)
    #expect(uiListNavigationDecision(key: .enter, primaryRole: .static) == nil)
    #expect(uiListNavigationDecision(key: .space, primaryRole: .toggle) == .invokePrimary)
    #expect(uiListNavigationDecision(key: .space, primaryRole: .checkmark) == .pageDown)
    #expect(uiListNavigationDecision(key: .back, primaryRole: .command) == .back)
}

@Test
func nativeMarkdownSelectionReconciliationPreservesOnlyUnsyncedLocalRanges() {
    let previous = NSRange(location: 2, length: 0)
    let incoming = NSRange(location: 2, length: 8)
    #expect(shouldApplyAuthoritativeMarkdownSelection(
        editorOwnsFocus: true,
        currentRange: previous,
        previousRange: previous,
        incomingRange: incoming
    ))
    #expect(shouldApplyAuthoritativeMarkdownSelection(
        editorOwnsFocus: true,
        currentRange: incoming,
        previousRange: previous,
        incomingRange: incoming
    ))
    #expect(!shouldApplyAuthoritativeMarkdownSelection(
        editorOwnsFocus: true,
        currentRange: NSRange(location: 5, length: 3),
        previousRange: previous,
        incomingRange: incoming
    ))
    #expect(shouldApplyAuthoritativeMarkdownSelection(
        editorOwnsFocus: false,
        currentRange: NSRange(location: 5, length: 3),
        previousRange: previous,
        incomingRange: incoming
    ))
}

@Test
func richListItemsDecodeBandsMediaDividersAndBusyFooters() throws {
    let json = """
    {"id":"usage-page","type":"page","title":"Usage","body":{"type":"list","id":"rows",
    "rowLayout":{"type":"auto","stackBelowWidth":60},
    "items":[
      {"id":"sep","label":"Providers","divider":true},
      {"id":"codex","label":"Codex","value":"42% left",
       "trailing":{"type":"gauge","id":"slot","ratio":0.42,"label":"Quota","accessibilityText":"42 percent left"},
       "bottom":{"type":"gauge","id":"band","ratio":0.42,"label":"Quota","accessibilityText":"42 percent left"},
       "top":{"type":"text","text":"Shipped","tone":"success"},
       "media":{"side":"trailing","width":4,"glyph":"CX","tone":"info"}}
    ]},
    "footer":{"actions":[{"id":"refresh","label":"refreshing…","action":"refresh","busy":true,"disabled":true}]}}
    """
    let node = try JSONDecoder().decode(UINode.self, from: Data(json.utf8))
    guard case let .page(page) = node.component, case let .list(list) = page.body else {
        Issue.record("expected a Page with a List body")
        return
    }
    #expect(list.rowLayout == .auto(stackBelowWidth: 60))
    #expect(list.items[0].divider)
    #expect(list.items[0].primaryRole == .static)
    let codex = list.items[1]
    #expect(codex.bottom?.id == "band")
    if case let .text(text, tone) = codex.top {
        #expect(text == "Shipped" && tone == .success)
    } else {
        Issue.record("expected a text band")
    }
    #expect(codex.media?.side == .trailing)
    #expect(codex.media?.width == 4)
    #expect(codex.media?.glyph == "CX")
    #expect(page.footer.actions[0].busy)
    let encoded = try JSONEncoder().encode(node)
    let decoded = try JSONDecoder().decode(UINode.self, from: encoded)
    #expect(decoded == node)
    #expect(throws: DecodingError.self) {
        try JSONDecoder().decode(
            UIListItemMedia.self,
            from: Data(#"{"width":40}"#.utf8)
        )
    }
}

@Test
func textBoxFixtureDecodesAndRoundTrips() throws {
    let testDirectory = URL(fileURLWithPath: #filePath).deletingLastPathComponent()
    let fixture = testDirectory
        .appendingPathComponent("../../../protocol/unpeel-ui-v1.ndjson")
        .standardizedFileURL
    let stream = try String(contentsOf: fixture, encoding: .utf8)
    let messages = try stream
        .split(separator: "\n")
        .map { try JSONDecoder().decode(UIMessage.self, from: Data($0.utf8)) }
    guard case let .snapshot(snapshot) = messages[44],
          case let .textBox(textBox) = snapshot.root.component
    else {
        Issue.record("text box fixture must start with a textBox snapshot")
        return
    }
    #expect(snapshot.root.id == "chat-prompt")
    #expect(textBox.prompt == "❯ ")
    #expect(textBox.titles == [TextBoxTitle(text: "Grok 4.6 (xhigh) · always-approve", position: .bottomRight)])
    #expect(textBox.hints.count == 2)
    #expect(textBox.submitMode == .enter)
    #expect(textBox.minRows == 3 && textBox.maxRows == 8)
    #expect(textBox.actions.submit == "submit")
    #expect(snapshot.root.component.requiredCapabilities == [UnpeelUIProtocol.textBoxCapability])
    guard case let .event(setText) = messages[45] else {
        Issue.record("text box fixture must carry a set-text event")
        return
    }
    #expect(setText.action.kind == .change)
    #expect(setText.action.value == .text("Ship it\n🙂 second line"))
    guard case let .delta(delta) = messages[46] else {
        Issue.record("text box fixture must carry a replaceRoot delta")
        return
    }
    let busySnapshot = try snapshot.applying(delta)
    guard case let .textBox(busyBox) = busySnapshot.root.component else {
        Issue.record("text box delta must preserve the textBox root")
        return
    }
    #expect(busyBox.busy?.elapsedMs == 8500)
    #expect(busyBox.text == "Ship it\n🙂 second line")
    let encoded = try JSONEncoder().encode(busySnapshot.root)
    let decoded = try JSONDecoder().decode(UINode.self, from: encoded)
    #expect(decoded == busySnapshot.root)
}

@Test
func sharedProtocolFixturesDecode() throws {
    let testDirectory = URL(fileURLWithPath: #filePath).deletingLastPathComponent()
    let fixture = testDirectory
        .appendingPathComponent("../../../protocol/unpeel-ui-v1.ndjson")
        .standardizedFileURL
    let stream = try String(contentsOf: fixture, encoding: .utf8)
    let messages = try stream
        .split(separator: "\n")
        .map { try JSONDecoder().decode(UIMessage.self, from: Data($0.utf8)) }

    #expect(messages.count == 53)
    guard case let .attach(attach) = messages[0] else {
        Issue.record("first fixture must attach an authenticated participant")
        return
    }
    #expect(attach.participantToken.hasPrefix("upui1."))
    #expect(attach.minProtocolVersion == 1)
    #expect(attach.maxProtocolVersion == 1)

    guard case let .snapshot(snapshot) = messages[2],
          case let .markdownEditor(editor) = snapshot.root.component
    else {
        Issue.record("third fixture must contain a Markdown editor snapshot")
        return
    }
    #expect(snapshot.revision == 7)
    #expect(editor.text == "# Hello\n🙂 world")
    #expect(editor.selection.head == UITextPosition(line: 1, utf16Column: 2))
    #expect(editor.presentation == .split)

    guard case let .presence(presence) = messages[3] else {
        Issue.record("fourth fixture must contain multi-user presence")
        return
    }
    #expect(presence.members.count == 2)

    guard case let .event(edit) = messages[4],
          case let .textEdit(value) = edit.action.value
    else {
        Issue.record("fourth fixture must contain a text edit")
        return
    }
    #expect(value.range.end.utf16Column == 2)
    #expect(value.text == "Hello")

    guard case let .delta(delta) = messages[10] else {
        Issue.record("eleventh fixture must contain the Markdown server delta")
        return
    }
    let updated = try snapshot.applying(delta)
    guard case let .markdownEditor(updatedEditor) = updated.root.component else {
        Issue.record("delta result must remain a Markdown editor")
        return
    }
    #expect(updated.revision == 8)
    #expect(updatedEditor.text == "# Hello\nHello world")
    #expect(updatedEditor.selection.head == UITextPosition(line: 1, utf16Column: 5))

    guard case let .snapshot(mediaSnapshot) = messages[11],
          case let .media(media) = mediaSnapshot.root.component
    else {
        Issue.record("twelfth fixture must contain Media")
        return
    }
    #expect(media.alt == "Tiny fixture pixel")
    #expect(media.resolvedPointSize.w == 40)
    #expect(media.resolvedPointSize.h == 40)

    guard case let .delta(mediaDelta) = messages[12] else {
        Issue.record("thirteenth fixture must contain a Media reference delta")
        return
    }
    let updatedMedia = try mediaSnapshot.applying(mediaDelta)
    guard case let .media(nextMedia) = updatedMedia.root.component,
          case let .blob(reference) = nextMedia.source
    else {
        Issue.record("Media delta must replace the source with a blob reference")
        return
    }
    #expect(reference.byteLength == 68)

    guard case let .snapshot(todoSnapshot) = messages[13],
          case let .page(page) = todoSnapshot.root.component,
          case let .list(list) = page.body,
          case let .input(input) = page.header,
          case let .toggle(toggle)? = list.items[1].trailing
    else {
        Issue.record("fourteenth fixture must contain the canonical Todo Page")
        return
    }
    #expect(page.title == "Todos")
    #expect(list.items.map(\.label) == [
        "Run the standalone TUI",
        "Attach SwiftUI or web",
        "Invite an agent with edit grant",
    ])
    #expect(input.submit == "add-todo")
    #expect(toggle.value == false)
    #expect(list.selectedID == "todo-1")
    #expect(list.select == "select-todo")
    #expect(page.requiredCapabilities == [
        "page", "list", "listItem", "input", "listItemRole", "toggle", "listSelection",
    ])

    guard case let .delta(todoDelta) = messages[14],
          case let .page(updatedPage) = try todoSnapshot.applying(todoDelta).root.component,
          case let .list(updatedList) = updatedPage.body
    else {
        Issue.record("fifteenth fixture must update Todo Page")
        return
    }
    #expect(updatedList.items[1].done)
    #expect(updatedList.selectedID == "todo-3")

    guard case let .snapshot(usageSnapshot) = messages[15],
          case let .page(usagePage) = usageSnapshot.root.component,
          case let .list(usageList) = usagePage.body
    else {
        Issue.record("sixteenth fixture must contain the Usage master/detail Page")
        return
    }
    #expect(usagePage.back == "close-provider")
    #expect(usageList.items[0].detail == "Resets in 6d 18h")
    #expect(usageList.items[0].value == "3% used")
    #expect(usageList.items[0].valueTone == .success)
    #expect(usageList.items[0].emphasis == .strong)
    guard case let .status(status)? = usageList.items[0].leading,
          case let .badge(badge)? = usageList.items[0].accessory
    else {
        Issue.record("Usage fixture must contain status and badge row slots")
        return
    }
    #expect(status.symbol == "✓")
    #expect(status.preserveToneWhenSelected)
    #expect(badge.text == "Pro")
    #expect(usageList.items[1].busy)
    #expect(usageList.items[1].activate == "refresh-usage")
    #expect(usagePage.requiredCapabilities == [
        "page", "list", "listItem", "pageBack", "listItemMetadata", "listItemActivate",
        "listItemRole", "listItemPresentation", "statusSymbol", "badge", "listSelection",
    ])

    guard case let .snapshot(surfaceSnapshot) = messages[16],
          case let .surface(surface) = surfaceSnapshot.root.component
    else {
        Issue.record("seventeenth fixture must contain the planet Surface")
        return
    }
    #expect(surface.reference.sessionID == "terminal-9")
    #expect(surface.reference.streamID == "planets")
    #expect(surface.inputPolicy == .pointerAndKeyboard)
    #expect(!UnpeelUIProtocol.supportedComponentCapabilities.contains(
        UnpeelUIProtocol.surfaceCapability
    ))
    let resolvedSurfaceSize = surface.resolvedPointSize(viewport: .init(w: 960, h: 600))
    #expect(resolvedSurfaceSize?.w == 960)
    #expect(resolvedSurfaceSize?.h == 600)

    guard case let .delta(surfaceDelta) = messages[17],
          case let .surface(updatedSurface) = try surfaceSnapshot
            .applying(surfaceDelta).root.component
    else {
        Issue.record("eighteenth fixture must switch the Surface reference")
        return
    }
    #expect(updatedSurface.reference.streamID == "planets-detail")

    guard case let .snapshot(canvasSnapshot) = messages[18],
          case let .canvasPage(canvas) = canvasSnapshot.root.component,
          case let .button(select) = canvas.controls[2],
          case let .event(canvasEvent) = messages[19]
    else {
        Issue.record("final fixtures must contain CanvasPage and a Button action")
        return
    }
    #expect(canvas.title == "Planet Canvas")
    #expect(canvas.surface.id == "planet-canvas")
    #expect(canvas.surface.surface.reference.streamID == "canvas-planets")
    #expect(canvas.requiredCapabilities == ["canvasPage", "surface", "button"])
    #expect(select.role == .primary)
    #expect(canvasEvent.action.nodeID == "canvas-next")
    #expect(canvasEvent.action.kind == .activate)
    guard case let .delta(canvasDelta) = messages[20],
          case let .canvasPage(updatedCanvas) = try canvasSnapshot
            .applying(canvasDelta).root.component
    else {
        Issue.record("Canvas Surface delta must preserve CanvasPage")
        return
    }
    #expect(updatedCanvas.surface.surface.reference.streamID == "canvas-planets-detail")

    guard case let .snapshot(rolesSnapshot) = messages[21],
          case let .page(rolesPage) = rolesSnapshot.root.component,
          case let .list(roles) = rolesPage.body,
          case let .delta(rolesDelta) = messages[22],
          case let .page(updatedRolesPage) = try rolesSnapshot.applying(rolesDelta).root.component,
          case let .list(updatedRoles) = updatedRolesPage.body
    else {
        Issue.record("final fixtures must contain every row role and its compact delta")
        return
    }
    #expect(roles.items.map(\.primaryRole) == [
        .toggle, .disclosure, .checkmark, .command, .destructive, .static,
    ])
    #expect(roles.items[4].actionRole == .destructive)
    #expect(rolesPage.requiredCapabilities == [
        "page", "list", "listItem", "pageBack", "listItemMetadata", "listItemActivate",
        "listItemRole", "toggle", "listItemPresentation", "listSelection",
    ])
    #expect(updatedRoles.items[2].primaryCheckmark?.value == false)
    #expect(updatedRoles.selectedID == "row-destructive")

    guard case let .snapshot(treeSnapshot) = messages[23],
          case let .tree(tree) = treeSnapshot.root.component,
          case let .delta(treeDelta) = messages[24],
          case let .tree(updatedTree) = try treeSnapshot.applying(treeDelta).root.component
    else {
        Issue.record("final fixtures must contain Tree hierarchy and keyed deltas")
        return
    }
    #expect(tree.location == "Writing")
    #expect(tree.selectedID == "today")
    #expect(tree.requiredCapabilities == [
        "tree", "treeHierarchy", "treeFilter", "treeParent", "button",
    ])
    #expect(tree.primaryAction?.action == "create-note")
    #expect(updatedTree.location == "Writing/Projects")
    #expect(updatedTree.filter?.value == "draft")
    #expect(updatedTree.selectedID == "draft")
    #expect(updatedTree.items[1].children.first?.label == "Draft.md")

    guard case let .snapshot(menuSnapshot) = messages[25],
          case let .menu(menu) = menuSnapshot.root.component,
          case let .delta(menuDelta) = messages[26],
          case let .menu(updatedMenu) = try menuSnapshot.applying(menuDelta).root.component,
          case let .snapshot(markdownMenuSnapshot) = messages[27],
          case let .markdownEditor(markdownWithMenus) = markdownMenuSnapshot.root.component,
          case let .delta(markdownMenuDelta) = messages[28],
          case let .markdownEditor(markdownWithoutInsert) = try markdownMenuSnapshot
            .applying(markdownMenuDelta).root.component
    else {
        Issue.record("final fixtures must contain root and nested semantic Menus")
        return
    }
    #expect(menu.requiredCapabilities == ["menu", "menuAnchor"])
    #expect(menu.items[1].disabled)
    #expect(menu.items[2].role == .danger)
    #expect(updatedMenu.selectedID == "delete")
    #expect(markdownWithMenus.insertMenu?.anchor == .caret)
    #expect(markdownMenuSnapshot.root.component.requiredCapabilities == [
        "markdownEditor", "menu", "menuAnchor",
    ])
    #expect(markdownWithoutInsert.insertMenu == nil)
    #expect(markdownWithoutInsert.contextMenu != nil)

    guard case let .snapshot(contentSnapshot) = messages[29],
          case let .page(contentPage) = contentSnapshot.root.component,
          case let .content(content) = contentPage.body,
          case let .delta(contentDelta) = messages[30],
          case let .page(updatedContentPage) = try contentSnapshot
            .applying(contentDelta).root.component,
          case let .content(updatedContent) = updatedContentPage.body
    else {
        Issue.record("final fixtures must contain read-only Content and compact deltas")
        return
    }
    #expect(content.lines.count == 3)
    #expect(content.selection?.anchorID == "line-1")
    #expect(contentPage.requiredCapabilities == [
        "page", "content", "pageBack", "contentSelection", "menu", "menuAnchor",
    ])
    #expect(updatedContent.lines[2].id == "line-2-next")
    #expect(updatedContent.selection?.headID == "line-2-next")

    guard case let .snapshot(hintSnapshot) = messages[31],
          case let .markdownEditor(hintEditor) = hintSnapshot.root.component,
          case let .event(openSlash) = messages[32],
          case let .delta(hintDelta) = messages[33],
          case let .markdownEditor(updatedHintEditor) = try hintSnapshot
            .applying(hintDelta).root.component
    else {
        Issue.record("final fixtures must carry one spec-owned Markdown hint vector")
        return
    }
    #expect(hintEditor.commandHintVisible)
    #expect(hintEditor.menuTrigger(forTextInput: "/") == .slash)
    #expect(hintSnapshot.root.component.requiredCapabilities == [
        "markdownEditor", "markdownCommandHint",
    ])
    #expect(openSlash.action.action == "open-menu")
    #expect(openSlash.action.value == .text("slash"))
    #expect(updatedHintEditor.commandHint?.text == "Type '/' for blocks")
    #expect(!updatedHintEditor.commandHintVisible)

    guard case let .snapshot(sparklineSnapshot) = messages[34],
          case let .page(sparklinePage) = sparklineSnapshot.root.component,
          case let .list(sparklineList) = sparklinePage.body,
          case let .sparkline(sparkline)? = sparklineList.items.first?.trailing
    else {
        Issue.record("last fixture must carry the shared Usage Sparkline vector")
        return
    }
    #expect(sparkline.series == [0, 3, 1.5, 4])
    #expect(sparkline.resolvedBounds == 0...5)
    #expect(sparkline.normalizedSeries == [0, 0.6, 0.3, 0.8])
    #expect(sparklinePage.requiredCapabilities == [
        "page", "list", "listItem", "listItemRole", "listItemPresentation", "sparkline",
    ])
    guard case let .delta(sparklineDelta) = messages[35],
          case let .page(updatedSparklinePage) = try sparklineSnapshot
            .applying(sparklineDelta).root.component,
          case let .list(updatedSparklineList) = updatedSparklinePage.body,
          case let .sparkline(updatedSparkline)? = updatedSparklineList.items.first?.trailing
    else {
        Issue.record("final fixture must update only the keyed Sparkline data")
        return
    }
    #expect(updatedSparkline.series == [1, 2, 5])
    #expect(updatedSparkline.resolvedBounds == 0...5)
    #expect(updatedSparkline.caption == "Latest trend")
    #expect(updatedSparkline.activate == "open-trend")

    guard case let .snapshot(barSnapshot) = messages[36],
          case let .page(barPage) = barSnapshot.root.component,
          case let .barChart(barChart) = barPage.body,
          case let .delta(barDelta) = messages[37],
          case let .page(updatedBarPage) = try barSnapshot.applying(barDelta).root.component,
          case let .barChart(updatedBarChart) = updatedBarPage.body
    else {
        Issue.record("BarChart fixture and keyed delta must decode")
        return
    }
    #expect(barChart.normalizedValues == [12.0 / 18.0, 1, 7.0 / 18.0])
    #expect(barPage.requiredCapabilities == ["page", "barChart"])
    #expect(updatedBarChart.bars[1].value == 20)
    #expect(updatedBarChart.activate == "next-chart")

    guard case let .snapshot(lineSnapshot) = messages[38],
          case let .page(linePage) = lineSnapshot.root.component,
          case let .lineChart(lineChart) = linePage.body,
          case let .delta(lineDelta) = messages[39],
          case let .page(updatedLinePage) = try lineSnapshot.applying(lineDelta).root.component,
          case let .lineChart(updatedLineChart) = updatedLinePage.body
    else {
        Issue.record("LineChart fixture and keyed delta must decode")
        return
    }
    #expect(lineChart.resolvedXBounds == 0...2)
    #expect(lineChart.resolvedYBounds == 0...8)
    #expect(linePage.requiredCapabilities == ["page", "lineChart"])
    #expect(updatedLineChart.series[0].points[2].y == 7)
    #expect(updatedLineChart.activate == "next-chart")

    guard case let .snapshot(gaugeSnapshot) = messages[40],
          case let .page(gaugePage) = gaugeSnapshot.root.component,
          case let .gauge(gauge) = gaugePage.body,
          case let .delta(gaugeDelta) = messages[41],
          case let .page(updatedGaugePage) = try gaugeSnapshot.applying(gaugeDelta).root.component,
          case let .gauge(updatedGauge) = updatedGaugePage.body
    else {
        Issue.record("Gauge fixture and keyed delta must decode")
        return
    }
    #expect(gauge.ratio == 0.64)
    #expect(gauge.percentageLabel == "Deployment  64%")
    #expect(UIGaugeSpec(
        id: "rounding-gauge",
        ratio: 0.625,
        label: "Build",
        accessibilityText: "Build is 62.5 percent complete"
    ).percentageLabel == "Build  63%")
    #expect(gaugePage.requiredCapabilities == ["page", "gauge"])
    #expect(updatedGauge.ratio == 0.82)
    #expect(updatedGauge.activate == "next-chart")

    guard case let .snapshot(quotaSnapshot) = messages[42],
          case let .page(quotaPage) = quotaSnapshot.root.component,
          case let .list(quotaList) = quotaPage.body,
          case let .gauge(quotaGauge)? = quotaList.items.first?.trailing,
          case let .delta(quotaDelta) = messages[43],
          case let .page(updatedQuotaPage) = try quotaSnapshot.applying(quotaDelta).root.component,
          case let .list(updatedQuotaList) = updatedQuotaPage.body,
          case let .gauge(updatedQuotaGauge)? = updatedQuotaList.items.first?.trailing
    else {
        Issue.record("List Gauge fixture and keyed delta must decode")
        return
    }
    #expect(quotaGauge.ratio == 0.77)
    #expect(quotaGauge.valueLabel == "77% left · Resets in 5d 14h")
    #expect(quotaPage.footer.actions[0].accelerator == "a")
    #expect(quotaPage.footer.actions[1].label == "refresh")
    #expect(quotaPage.requiredCapabilities == [
        "page", "footerActions", "list", "listItem", "listItemPresentation", "gauge",
    ])
    #expect(updatedQuotaGauge.ratio == 0.61)
    #expect(updatedQuotaGauge.valueLabel == "61% left · Resets in 4d")
    #expect(updatedQuotaPage.footer.actions[1].label == "refreshing…")
    #expect(updatedQuotaPage.footer.actions[1].disabled)
}

@Test
func eventEncodingUsesTheWireEnvelope() throws {
    let snapshot = UISnapshot(
        appInstanceID: "app-fixture",
        clientID: "client-alice-mac",
        viewID: "main",
        revision: 3,
        root: UINode(
            id: "editor",
            component: .markdownEditor(MarkdownEditorSpec(
                text: "hello",
                selection: .caret(UITextPosition(line: 0, utf16Column: 0))
            ))
        )
    )
    let event = UIEvent(
        snapshot: snapshot,
        participantID: "person-alice",
        rendererID: "renderer-alice-swift",
        eventID: "event-save-1",
        action: UIAction(nodeID: "editor", action: "save", kind: .command)
    )
    let data = try JSONEncoder().encode(UIMessage.event(event))
    let object = try #require(JSONSerialization.jsonObject(with: data) as? [String: Any])
    #expect(object["type"] as? String == "event")
    #expect(object["protocol"] as? String == "unpeel.ui")
    #expect(object["appInstanceId"] as? String == "app-fixture")
    #expect(object["participantId"] as? String == "person-alice")
    #expect(object["eventId"] as? String == "event-save-1")
    #expect(object["baseRevision"] as? Int == 3)
    #expect(object["nodeId"] as? String == "editor")
    #expect((object["value"] as? [String: Any])?["type"] as? String == "none")
}

@Test
func unsupportedProtocolVersionIsRejected() {
    let frame = Data(
        #"{"type":"error","protocol":"unpeel.ui","protocolVersion":2,"code":"test","message":"test"}"#.utf8
    )
    #expect(throws: DecodingError.self) {
        try JSONDecoder().decode(UIMessage.self, from: frame)
    }
}

@Test
func attachUsesVersionRangeNegotiation() throws {
    let attach = UIAttach(
        participantToken: "upui1.payload.signature",
        clientID: "client-1",
        renderer: UIRendererMetadata(id: "renderer-1", kind: "swiftUI"),
        viewID: "main",
        minProtocolVersion: 2,
        maxProtocolVersion: 3
    )
    let data = try JSONEncoder().encode(UIMessage.attach(attach))
    let object = try #require(JSONSerialization.jsonObject(with: data) as? [String: Any])
    #expect(object["protocolVersion"] == nil)
    #expect(object["minProtocolVersion"] as? Int == 2)
    #expect(object["maxProtocolVersion"] as? Int == 3)
    #expect(try JSONDecoder().decode(UIMessage.self, from: data) == .attach(attach))
    #expect(UnpeelUIProtocol.negotiate(minimum: 1, maximum: 3) == 1)
    #expect(UnpeelUIProtocol.negotiate(minimum: 2, maximum: 3) == nil)
}

@Test
func nativeHostMintsRouteBoundAgentCredentials() throws {
    let issuer = try UIParticipantTokenIssuer(
        signingKey: "0123456789abcdef0123456789abcdef",
        appSessionID: "app-session"
    )
    let token = try issuer.issue(
        participant: UIParticipant(
            id: "agent:neighbor",
            kind: .agent,
            sourceSessionID: "neighbor-session",
            displayName: "Review agent",
            grants: ["view", "edit"]
        ),
        clientID: "client-agent",
        rendererID: "renderer-agent",
        viewID: "main",
        tokenID: "token-1",
        validFor: 60,
        now: Date(timeIntervalSince1970: 1_000)
    )
    #expect(token.hasPrefix("upui1."))
    #expect(token.split(separator: ".").count == 3)
    #expect(!token.contains("admin"))
}

@Test
func recognizedMessagesIgnoreUnknownFields() throws {
    let fixture: [String: Any] = [
        "type": "snapshot",
        "protocol": "unpeel.ui",
        "protocolVersion": 1,
        "appInstanceId": "app-fixture",
        "clientId": "client-1",
        "viewId": "main",
        "revision": 1,
        "futureEnvelopeField": ["v": 2],
        "root": [
            "id": "editor",
            "type": "markdownEditor",
            "text": "hello",
            "selection": [
                "anchor": ["line": 0, "utf16Column": 0],
                "head": ["line": 0, "utf16Column": 0],
                "futureSelectionField": true,
            ],
            "futureComponentField": "ignored",
        ],
    ]
    let data = try JSONSerialization.data(withJSONObject: fixture)
    guard case let .snapshot(snapshot) = try JSONDecoder().decode(UIMessage.self, from: data) else {
        Issue.record("recognized snapshot should decode")
        return
    }
    #expect(snapshot.root.id == "editor")
}

@Test
func unknownComponentDecodesForTerminalFallbackWithoutRejectingAttachment() throws {
    let fixture: [String: Any] = [
        "type": "snapshot",
        "protocol": "unpeel.ui",
        "protocolVersion": 1,
        "appInstanceId": "app-fixture",
        "clientId": "client-1",
        "viewId": "main",
        "revision": 1,
        "root": [
            "id": "future-root",
            "type": "futureComponent",
            "privatePayload": ["ignored": true],
        ],
    ]
    let data = try JSONSerialization.data(withJSONObject: fixture)
    guard case let .snapshot(snapshot) = try JSONDecoder().decode(UIMessage.self, from: data),
          case let .unsupported(kind) = snapshot.root.component
    else {
        Issue.record("unknown component must remain a decodable snapshot")
        return
    }
    #expect(kind == "futureComponent")
    #expect(snapshot.root.component.requiredCapability == nil)
}

@Test
func unknownPageSlotDecodesAndRequiresTerminalFallback() throws {
    let frame = Data(#"{"type":"snapshot","protocol":"unpeel.ui","protocolVersion":1,"appInstanceId":"app-fixture","clientId":"client-1","viewId":"main","revision":1,"root":{"id":"page","type":"page","title":"Future Page","body":{"type":"list","id":"rows","items":[{"id":"row-1","label":"Row","trailing":{"type":"futureControl","id":"control-1"}}]}}}"#.utf8)
    guard case let .snapshot(snapshot) = try JSONDecoder().decode(UIMessage.self, from: frame),
          case let .page(page) = snapshot.root.component,
          case let .list(list) = page.body,
          case let .unsupported(kind)? = list.items[0].trailing
    else {
        Issue.record("future row slot should stay decodable")
        return
    }
    #expect(kind == "futureControl")
    #expect(page.requiredCapabilities == nil)
    #expect(snapshot.root.component.requiredCapability == nil)
}

@Test
func pageInputAndListDeltasPreserveTheClosedRoot() throws {
    let first = UIListItemSpec(id: "todo-1", label: "First")
    let snapshot = UISnapshot(
        appInstanceID: "app-fixture",
        clientID: "client-1",
        viewID: "main",
        revision: 1,
        root: UINode(
            id: "todo-page",
            component: .page(PageSpec(
                title: "Todos",
                header: .input(UIInputSpec(id: "new-todo", label: "New todo")),
                body: .list(UIListSpec(id: "todos", items: [first]))
            ))
        )
    )
    let delta = UIDelta(
        appInstanceID: "app-fixture",
        clientID: "client-1",
        viewID: "main",
        baseRevision: 1,
        revision: 2,
        operations: [
            .inputSetValue(nodeID: "new-todo", value: "draft"),
            .listInsertItem(
                listID: "todos",
                index: 1,
                item: UIListItemSpec(id: "todo-2", label: "Second")
            ),
            .listRemoveItem(listID: "todos", itemID: "todo-1"),
            .listSetSelection(listID: "todos", selectedID: "todo-2"),
        ]
    )
    guard case let .page(page) = try snapshot.applying(delta).root.component,
          case let .input(input)? = page.header,
          case let .list(list) = page.body
    else {
        Issue.record("Page deltas should preserve the Page root")
        return
    }
    #expect(input.value == "draft")
    #expect(list.items.map(\.id) == ["todo-2"])
    #expect(list.selectedID == "todo-2")
}

@Test
func mediaPointSizingUsesExactIntegerAspectMath() {
    let media = MediaSpec(
        source: .path("/tmp/image.png"),
        intrinsic: MediaPixelSize(w: 4_294_967_291, h: 4_294_967_279),
        points: MediaPointSize(w: 4_294_967_283),
        alt: "Large dimensions"
    )
    #expect(media.resolvedPointSize.w == 4_294_967_283)
    #expect(media.resolvedPointSize.h == 4_294_967_272)
}

@Test
func mediaRejectsNoncanonicalInlineBytesAndActions() {
    let inline = Data(
        #"{"kind":"inline","mediaType":"image/png","base64":"AB=="}"#.utf8
    )
    #expect(throws: DecodingError.self) {
        try JSONDecoder().decode(MediaSource.self, from: inline)
    }

    let invalidAction = Data(
        #"{"source":{"kind":"path","path":"/tmp/image.png"},"intrinsic":{"w":1,"h":1},"alt":"Image","activate":"not portable"}"#.utf8
    )
    #expect(throws: DecodingError.self) {
        try JSONDecoder().decode(MediaSpec.self, from: invalidAction)
    }
}
