import XCTest
@testable import UnpeelNative

final class LicenseManagerTests: XCTestCase {
    private let payload = LicensePayload(
        v: 1,
        id: "lic_test",
        email: "test@example.com",
        plan: "personal",
        seats: 1,
        iat: 1_700_000_000
    )

    func testLicenseConfigUsesBundledProductionDefaults() {
        XCTAssertEqual(
            LicenseConfig.publicKeyBase64(environment: [:]),
            LicenseConfig.bundledPublicKeyBase64
        )
        XCTAssertEqual(
            LicenseConfig.apiBaseURL(environment: [:]),
            URL(string: "https://unpeel.com")!
        )
    }

    func testNormalizeLicenseKeyRepairsSmartDashesAndWhitespace() {
        let clean = "CLRTY-eyJhIjoxfQ.c2ln-Zm9v_YmFy"
        // macOS smart-dash substitution turns every hyphen into an en-dash;
        // paste can also inject wrapping newlines / stray spaces.
        let mangled = "CLRTY\u{2013}eyJhIjoxfQ.c2ln\u{2013}Zm9v_YmFy"
        XCTAssertEqual(LicenseManager.normalizeLicenseKey(mangled), clean)

        let withWhitespace = "  CLRTY-eyJhIjoxfQ.\nc2ln-Zm9v_YmFy  "
        XCTAssertEqual(LicenseManager.normalizeLicenseKey(withWhitespace), clean)

        // Em-dash, figure-dash, fullwidth hyphen, NBSP, zero-width space.
        let exotic = "CLRTY\u{2014}eyJhIjoxfQ.\u{00A0}c2ln\u{FF0D}Zm9v_YmF\u{200B}y"
        XCTAssertEqual(LicenseManager.normalizeLicenseKey(exotic), clean)

        // A well-formed key is unchanged (idempotent).
        XCTAssertEqual(LicenseManager.normalizeLicenseKey(clean), clean)
    }

    func testLicenseConfigUsesDevEnvironmentOverrides() {
        let environment = [
            "UNPEEL_LICENSE_PUBLIC_KEY": "dev-public-key",
            "UNPEEL_LICENSE_API_BASE_URL": "http://localhost:5173"
        ]

        XCTAssertEqual(
            LicenseConfig.publicKeyBase64(environment: environment),
            "dev-public-key"
        )
        XCTAssertEqual(
            LicenseConfig.apiBaseURL(environment: environment),
            URL(string: "http://localhost:5173")!
        )
    }

    func testLicenseConfigIgnoresInvalidAPIOverride() {
        XCTAssertEqual(
            LicenseConfig.apiBaseURL(environment: ["UNPEEL_LICENSE_API_BASE_URL": "not a url"]),
            URL(string: "https://unpeel.com")!
        )
    }

    func testDevelopmentBuildLicenseBypassReadsInfoPlistMarker() {
        XCTAssertTrue(
            LicenseConfig.developmentBuildLicenseBypassEnabled(
                infoDictionary: [LicenseConfig.developmentBuildInfoPlistKey: true]
            )
        )
        XCTAssertTrue(
            LicenseConfig.developmentBuildLicenseBypassEnabled(
                infoDictionary: [LicenseConfig.developmentBuildInfoPlistKey: "yes"]
            )
        )
        XCTAssertFalse(LicenseConfig.developmentBuildLicenseBypassEnabled(infoDictionary: [:]))
    }

    func testValidationResponseRequiresExplicitValidOrRevokedPayload() {
        XCTAssertEqual(
            LicenseManager.validationResponse(for: ["valid": true, "status": "active"]),
            .active
        )
        XCTAssertEqual(
            LicenseManager.validationResponse(for: ["valid": false, "status": "revoked"]),
            .revoked
        )
        XCTAssertEqual(
            LicenseManager.validationResponse(for: ["valid": false, "status": "unknown"]),
            .invalid
        )
        XCTAssertEqual(
            LicenseManager.validationResponse(for: [:]),
            .invalid
        )
    }

    func testSeatLimitMessageUsesPurchasedSeatCountWhenPresent() {
        XCTAssertEqual(
            LicenseManager.message(for: ["error": "seat_limit", "seats": 2]),
            "All 2 seats for this license are in use. Deactivate Unpeel on another Mac first, or contact support."
        )
    }

    func testSeatLimitMessageAvoidsOldThreeMacFallback() {
        XCTAssertEqual(
            LicenseManager.message(for: ["error": "seat_limit"]),
            "All purchased seats for this license are in use. Deactivate Unpeel on another Mac first, or contact support."
        )
    }

    /// Pro follows the license state alone: active unlocks it, unlicensed and
    /// revoked are both just "free" (the app itself is never gated).
    func testProFollowsLicenseStateOnly() {
        XCTAssertTrue(LicenseState.active(payload).isActive)
        XCTAssertFalse(LicenseState.unlicensed.isActive)
        XCTAssertFalse(LicenseState.revoked(payload).isActive)
    }

    func testDefinitiveRevocationSurvivesStoredLicenseRestore() {
        let payload = LicensePayload(
            v: 1,
            id: "license-1",
            email: "person@example.com",
            plan: "pro",
            seats: 1,
            iat: 1
        )

        XCTAssertEqual(
            LicenseManager.restoredState(payload: payload, revokedLicenseID: "license-1"),
            .revoked(payload)
        )
        XCTAssertEqual(
            LicenseManager.restoredState(payload: payload, revokedLicenseID: "another-license"),
            .active(payload)
        )
    }

    func testLateRevalidationCannotOverwriteDeactivateOrNewerKey() {
        XCTAssertFalse(
            LicenseManager.revalidationResponseIsCurrent(
                requestedKey: "old-key",
                currentKey: nil,
                requestGeneration: 4,
                currentGeneration: 5
            )
        )
        XCTAssertFalse(
            LicenseManager.revalidationResponseIsCurrent(
                requestedKey: "old-key",
                currentKey: "new-key",
                requestGeneration: 4,
                currentGeneration: 4
            )
        )
        XCTAssertTrue(
            LicenseManager.revalidationResponseIsCurrent(
                requestedKey: "same-key",
                currentKey: "same-key",
                requestGeneration: 4,
                currentGeneration: 4
            )
        )
    }
}
