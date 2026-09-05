//
//  main.swift
//  UnpeelNative
//
//  Plain-executable AppKit bootstrap (no .app bundle needed for the spike).
//

import AppKit

let app = NSApplication.shared
let delegate = AppDelegate()
app.delegate = delegate
// Regular activation policy so the bare executable gets a Dock icon,
// key-window status and keyboard focus.
app.setActivationPolicy(.regular)
// Bare executables have no .app bundle Info.plist to supply an icon, so set
// the Dock icon from the SwiftPM resource bundle instead. Skip this when the
// .app bundle already declares an icon (CFBundleIconFile/Name): overriding it
// with the raw full-bleed PNG makes the *running* Dock icon differ from the
// not-running icon (the .icns, which macOS draws with its standard margin).
let bundleDeclaresIcon = Bundle.main.object(forInfoDictionaryKey: "CFBundleIconFile") != nil
    || Bundle.main.object(forInfoDictionaryKey: "CFBundleIconName") != nil
if !bundleDeclaresIcon,
   let iconURL = ModuleResources.url(forResource: "AppIcon", withExtension: "png"),
   let icon = NSImage(contentsOf: iconURL) {
    app.applicationIconImage = icon
}
app.run()
