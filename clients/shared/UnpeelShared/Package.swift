// swift-tools-version: 6.0

import PackageDescription

let package = Package(
    name: "UnpeelShared",
    platforms: [
        .iOS(.v16),
        .macOS(.v13),
    ],
    products: [
        .library(
            name: "UnpeelShared",
            targets: ["UnpeelShared"]
        ),
    ],
    targets: [
        .target(
            name: "UnpeelShared",
            path: "Sources/UnpeelShared"
        ),
        .testTarget(
            name: "UnpeelSharedTests",
            dependencies: ["UnpeelShared"],
            path: "Tests/UnpeelSharedTests"
        ),
    ]
)
