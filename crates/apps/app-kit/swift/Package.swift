// swift-tools-version: 6.0

import PackageDescription

let package = Package(
    name: "UnpeelAppKitUI",
    platforms: [
        .macOS(.v14),
    ],
    products: [
        .library(name: "UnpeelAppKitUI", targets: ["UnpeelAppKitUI"]),
    ],
    targets: [
        .target(name: "UnpeelAppKitUI"),
        .testTarget(name: "UnpeelAppKitUITests", dependencies: ["UnpeelAppKitUI"]),
    ]
)
