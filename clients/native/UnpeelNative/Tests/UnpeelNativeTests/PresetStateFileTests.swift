import XCTest
@testable import UnpeelNative

final class PresetStateFileTests: XCTestCase {
    private func row(
        _ id: String, label: String, command: String, star: Bool = false, project: String? = nil
    ) -> [String: Any] {
        var row: [String: Any] = [
            "id": id, "label": label, "command": command, "enabled": true, "quick_launch": star,
        ]
        row["project_id"] = project ?? NSNull()
        return row
    }

    func testExactDuplicatesCollapseToTheFirstRowAndKeepOrder() {
        let rows = [
            row("a", label: "claude", command: "claude"),
            row("fx1", label: "fx", command: "fx"),
            row("b", label: "codex", command: "codex"),
            row("fx2", label: "fx", command: "fx"),
            row("fx3", label: "fx", command: "fx"),
        ]
        let result = PresetStateFile.collapseExactDuplicates(rows)
        XCTAssertEqual(result.removed, 2)
        XCTAssertEqual(result.rows.map { $0["id"] as? String }, ["a", "fx1", "b"])
    }

    func testAStarOnADroppedCopyCarriesOverToTheKeptRow() {
        let result = PresetStateFile.collapseExactDuplicates([
            row("fx1", label: "fx", command: "fx"),
            row("fx2", label: "fx", command: "fx", star: true),
        ])
        XCTAssertEqual(result.removed, 1)
        XCTAssertEqual(result.rows.first?["quick_launch"] as? Bool, true)
    }

    func testRowsThatDifferInLabelCommandOrProjectAreNeverCollapsed() {
        let rows = [
            row("1", label: "fx", command: "fx"),
            row("2", label: "fx (verbose)", command: "fx"),
            row("3", label: "fx", command: "fx --help"),
            row("4", label: "fx", command: "fx", project: "proj"),
            row("5", label: "", command: ""),
            row("6", label: "", command: ""),
        ]
        let result = PresetStateFile.collapseExactDuplicates(rows)
        XCTAssertEqual(result.removed, 0)
        XCTAssertEqual(result.rows.count, rows.count)
    }

    func testCollapsePreservesUnmodelledKeys() {
        var extra = row("fx1", label: "fx", command: "fx")
        extra["tauri_era_key"] = "keep me"
        let result = PresetStateFile.collapseExactDuplicates([extra, row("fx2", label: "fx", command: "fx")])
        XCTAssertEqual(result.rows.first?["tauri_era_key"] as? String, "keep me")
    }

    /// Client-mode Local scope (Host service on) does not rebuild the disk
    /// projection, but a rescan must still reload the shared preset file the
    /// app itself writes — otherwise "Agents you can add" keeps offering a
    /// just-added agent (the 0.4.0 duplicate-Add bug).
    func testHostServiceClientRescanStillReloadsTheSharedPresetFile() {
        XCTAssertEqual(
            UnpeelStore.presetReloadPlan(appliesLocalDiskProjection: false),
            .sharedPresetFileOnly
        )
        XCTAssertEqual(
            UnpeelStore.presetReloadPlan(appliesLocalDiskProjection: true),
            .fullDiskProjection
        )
    }
}
