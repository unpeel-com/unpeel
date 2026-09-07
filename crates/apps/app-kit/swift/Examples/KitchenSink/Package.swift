// swift-tools-version: 6.0

import Foundation
import PackageDescription

let surfacePackage = URL(fileURLWithPath: #filePath)
    .deletingLastPathComponent()
    .appendingPathComponent("../../../../unpeel-surface/swift/UnpeelSurfaceKit")
    .standardizedFileURL
let surfaceBinary = surfacePackage
    .appendingPathComponent("../../target/embed/UnpeelSurface.xcframework")
    .standardizedFileURL
let surfaceKitDisabled = ProcessInfo.processInfo.environment[
    "UNPEEL_KITCHEN_DISABLE_SURFACE_KIT"
] == "1"
let hasSurfaceKit = !surfaceKitDisabled && FileManager.default.fileExists(
    atPath: surfacePackage.appendingPathComponent("Package.swift").path
) && FileManager.default.fileExists(atPath: surfaceBinary.path)

var packageDependencies: [Package.Dependency] = [
    // This executable consumes the renderer library; the library never
    // depends on the harness.
    .package(path: "../.."),
    .package(
        url: "https://github.com/Lakr233/libghostty-spm.git",
        exact: "1.5.0"
    ),
]
var targetDependencies: [Target.Dependency] = [
    .product(name: "UnpeelAppKitUI", package: "swift"),
    .product(name: "GhosttyTerminal", package: "libghostty-spm"),
]
var swiftSettings: [SwiftSetting] = []
if hasSurfaceKit {
    packageDependencies.append(.package(path: surfacePackage.path))
    targetDependencies.append(
        .product(name: "UnpeelSurfaceKit", package: "UnpeelSurfaceKit")
    )
    swiftSettings.append(.define("UNPEEL_SURFACE_KIT"))
}

let package = Package(
    name: "UnpeelAppKitKitchenSink",
    platforms: [
        .macOS(.v14),
    ],
    products: [
        .executable(name: "KitchenSink", targets: ["KitchenSink"]),
    ],
    dependencies: packageDependencies,
    targets: [
        .executableTarget(
            name: "KitchenSink",
            dependencies: targetDependencies,
            resources: [.copy("Resources/Web")],
            swiftSettings: swiftSettings
        ),
    ]
)
