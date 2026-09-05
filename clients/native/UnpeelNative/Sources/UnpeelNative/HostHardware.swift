import Foundation

/// This Mac's hardware family, resolved once from `sysctl hw.model` and cached.
/// Controllers advertise this in the bootstrap (`hostDeviceKind`/
/// `hostDeviceModel`) so a remote Host shows the right icon and model hint
/// (a MacBook vs a Mac Studio vs a Linux box). Presentation only — never gate
/// behavior on it.
///
/// There is no public API for the marketing name, so this maps the model
/// identifier *family* prefix to a stable kind string and a coarse family
/// label ("MacBook Pro" vs "MacBook" is not distinguished — a per-board table
/// is deliberately avoided). Unknown Macs still report a Mac model identifier.
enum HostHardware {
    /// Stable kind string matching `RemoteBootstrapSnapshot.hostDeviceKind`:
    /// "macbook" | "macMini" | "macStudio" | "imac" | "macPro" | "unknown".
    /// (A Linux host reports "linux" from the Rust bootstrap, not here.)
    static var deviceKind: String { cached.kind }

    /// Human-readable model hint, e.g. "MacBook", "Mac Studio", or the raw
    /// model identifier when the family is unrecognized.
    static var deviceModel: String { cached.model }

    private static let cached: (kind: String, model: String) = resolve()

    /// `hw.model` — the model identifier, e.g. "Mac14,7" or "Macmini9,1".
    private static func modelIdentifier() -> String? {
        var size = 0
        guard sysctlbyname("hw.model", nil, &size, nil, 0) == 0, size > 0 else {
            return nil
        }
        var buffer = [UInt8](repeating: 0, count: size)
        guard sysctlbyname("hw.model", &buffer, &size, nil, 0) == 0 else {
            return nil
        }
        // sysctl null-terminates the string; drop the trailing NUL(s).
        let bytes = buffer.prefix { $0 != 0 }
        let value = String(decoding: bytes, as: UTF8.self)
        return value.isEmpty ? nil : value
    }

    private static func resolve() -> (kind: String, model: String) {
        guard let identifier = modelIdentifier() else {
            return ("unknown", "Mac")
        }
        // Apple Silicon Studio ships as "Mac13,1"/"Mac13,2" and
        // "Mac14,13"/"Mac14,14"; the base Mac Pro is "Mac14,8".
        let studioIDs: Set<String> = ["Mac13,1", "Mac13,2", "Mac14,13", "Mac14,14"]
        if studioIDs.contains(identifier) {
            return ("macStudio", "Mac Studio")
        }
        if identifier == "Mac14,8" {
            return ("macPro", "Mac Pro")
        }
        if identifier.hasPrefix("MacBook") {
            return ("macbook", "MacBook")
        }
        if identifier.hasPrefix("Macmini") {
            return ("macMini", "Mac mini")
        }
        if identifier.hasPrefix("iMac") {
            return ("imac", "iMac")
        }
        if identifier.hasPrefix("MacPro") {
            return ("macPro", "Mac Pro")
        }
        // A newer/unrecognized Apple Silicon board ("MacX,Y") is still a Mac,
        // but we cannot name its family — report unknown with the raw id.
        return ("unknown", identifier)
    }
}
