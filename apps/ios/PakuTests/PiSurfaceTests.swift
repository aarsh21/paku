import SwiftUI
import UIKit
import XCTest
@testable import Paku

final class PiSurfaceTests: XCTestCase {
    private let removed = ["claude-code", "codex", "cursor", "devin", "grok", "hermes", "opencode", "antigravity"]

    func testNewDraftDefaultsToPi() {
        XCTAssertEqual(NewSessionDraft().harness, "pi")
    }

    func testRestoredLegacyDraftKeepsWorkspaceButDropsAgentSpecificChoices() throws {
        for harness in removed + ["unknown"] {
            let data = try JSONSerialization.data(withJSONObject: [
                "harness": harness, "model": "old-model", "effort": "ultrathink",
                "projectId": "project", "hostId": "host", "branch": "feature", "worktree": true,
            ])
            let restored = try JSONDecoder().decode(NewSessionDraft.self, from: data).normalizedHarness()
            XCTAssertEqual(restored.harness, "pi")
            XCTAssertNil(restored.model)
            XCTAssertNil(restored.effort)
            XCTAssertEqual(restored.projectId, "project")
            XCTAssertEqual(restored.hostId, "host")
            XCTAssertEqual(restored.branch, "feature")
            XCTAssertTrue(restored.worktree)
        }
    }

    func testPiProviderModelsAndExplicitMockSurviveDraftRoundTrip() throws {
        for harness in ["pi", "mock"] {
            for model in ["anthropic/claude-opus-4-5", "openai/gpt-5.4", "google/gemini-2.5-pro"] {
                var draft = NewSessionDraft()
                draft.harness = harness
                draft.model = model
                draft.effort = "high"
                let restored = try JSONDecoder().decode(NewSessionDraft.self, from: JSONEncoder().encode(draft))
                XCTAssertEqual(restored.normalizedHarness(), draft)
            }
        }
    }

    func testPickerRejectsLegacyCatalogAndHidesMock() {
        func info(_ id: String, offered: Bool = true) -> HarnessInfo {
            HarnessInfo(id: id, label: id, supportsSteering: nil, steeringMode: nil,
                        reasoningLevels: [], installed: offered, enabled: nil,
                        offered: offered, midTurnSteering: nil)
        }
        let oldHost = (removed + ["mock", "pi"]).map { info($0) }
        XCTAssertEqual(HarnessNames.offered(oldHost).map(\.id), ["pi"])
        XCTAssertTrue(HarnessNames.offered([info("pi", offered: false)]).isEmpty)
        XCTAssertEqual(fallbackHarnesses().map(\.id), ["pi"])
    }

    func testMarksNeverFallBackToRemovedAgentBrand() {
        XCTAssertEqual(BrandMark.forHarness("pi"), .pi)
        XCTAssertEqual(BrandMark.forHarness("mock"), .pi)
        XCTAssertEqual(BrandMarks.image(for: "pi")?.renderingMode, .alwaysTemplate)
        for id in removed + ["unknown", ""] {
            XCTAssertNil(BrandMark.forHarness(id))
            XCTAssertNil(BrandMarks.image(for: id))
        }
        XCTAssertNil(BrandMarks.image(for: nil))
        // Preserve the Pi SVG's cutout rather than filling it as a solid block.
        let path = BrandMarkShape(mark: .pi).path(in: CGRect(x: 0, y: 0, width: 800, height: 800)).cgPath
        XCTAssertTrue(path.contains(CGPoint(x: 200, y: 200), using: .evenOdd))
        XCTAssertFalse(path.contains(CGPoint(x: 300, y: 300), using: .evenOdd))
        XCTAssertTrue(path.contains(CGPoint(x: 550, y: 550), using: .evenOdd))
    }

    func testFixtureUsesPiButStillOffersMultipleModelProviders() {
        let source = FixtureSessionSource(title: "Fixture", subtitle: "Offline")
        XCTAssertEqual(source.chrome.placeholder, "Message Pi")
        let actions = source.chipMenu("model")?.children.compactMap { $0 as? UIAction } ?? []
        XCTAssertEqual(actions.map(\.title), ["Opus 4.5", "GPT-5.4", "Gemini 2.5 Pro"])
        XCTAssertEqual(source.chrome.chips.first { $0.id == "model" }?.icon?.renderingMode, .alwaysTemplate)
    }
}
