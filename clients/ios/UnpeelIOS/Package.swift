// swift-tools-version: 6.0

import PackageDescription

let package = Package(
    name: "UnpeelIOS",
    platforms: [
        .iOS(.v17),
    ],
    products: [
        .library(
            name: "UnpeelIOS",
            targets: ["UnpeelIOS"]
        ),
    ],
    dependencies: [
        .package(path: "../../shared/UnpeelShared"),
        .package(path: "../../native/vendor/libghostty-spm"),
    ],
    targets: [
        .target(
            name: "UnpeelIOS",
            dependencies: [
                .product(name: "UnpeelShared", package: "UnpeelShared"),
                .product(name: "GhosttyTerminal", package: "libghostty-spm"),
            ],
            path: "Sources/UnpeelIOS"
        ),
        .testTarget(
            name: "UnpeelIOSTests",
            dependencies: ["UnpeelIOS"],
            path: "Tests/UnpeelIOSTests"
        ),
    ]
)
