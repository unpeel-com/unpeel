import Foundation

/// Safe lookup for files in the SwiftPM resource bundle
/// (`UnpeelNative_UnpeelNative.bundle`).
///
/// Never use `Bundle.module` in this target: for *executable* targets SwiftPM
/// generates an accessor that only checks `Bundle.main.bundleURL` (the .app
/// root — but `build-app.sh` ships the bundle in `Contents/Resources/`) and
/// then an absolute `.build` path that only exists on the build machine, and
/// `fatalError`s when both miss. That crashed Settings ▸ Remote on every
/// install except the build Mac (beta.6–beta.25).
enum ModuleResources {
    private static let bundleName = "UnpeelNative_UnpeelNative.bundle"

    static func url(forResource name: String, withExtension ext: String) -> URL? {
        var dirs: [URL] = []
        if let resources = Bundle.main.resourceURL {
            dirs.append(resources) // packaged app: Contents/Resources/
        }
        dirs.append(Bundle.main.bundleURL) // bare executable: next to the binary in .build/

        for dir in dirs {
            let url = dir
                .appendingPathComponent(bundleName)
                .appendingPathComponent("\(name).\(ext)")
            if FileManager.default.fileExists(atPath: url.path) {
                return url
            }
        }
        return nil
    }
}
