//
//  Localization.swift
//  UnpeelNative
//
//  App UI language preference (Settings ▸ Appearance ▸ Language) over Apple
//  String Catalog lookups. Source of truth:
//  `UnpeelNative/Localization/Localizable.xcstrings`, compiled to
//  `Resources/zh-Hans.lproj` by `clients/native/sync-localizations.sh` and
//  loaded from the SwiftPM resource bundle via ModuleResources. Native-only
//  UserDefaults overlay — never written to app-state.json. English source
//  strings are the lookup keys; missing translations fall back to English.
//
//  Lookups go through ModuleResources (never Bundle.module) so packaged
//  .app binaries resolve the same catalog that `swift build` embeds.
//

import Foundation
import SwiftUI

/// Settings ▸ Appearance ▸ Language. `.system` follows macOS preferred languages.
enum AppLanguage: String, CaseIterable, Identifiable {
    case system
    case english
    case simplifiedChinese

    var id: String { rawValue }

    /// Picker labels: English / 简体中文 keep their native spelling; System
    /// follows the active UI language.
    var displayName: String {
        switch self {
        case .system: return L10n.t("System")
        case .english: return "English"
        case .simplifiedChinese: return "简体中文"
        }
    }
}

enum AppLocaleCode: String, Sendable {
    case english = "en"
    case simplifiedChinese = "zh-Hans"

    var lprojName: String { rawValue }
}

/// Thread-safe string lookup. Preference mutations publish on the main actor
/// via `LocalizationStore`; the resolved code here is a snapshot for any
/// caller (menu validation, model `title` getters, SwiftUI bodies).
enum L10n {
    private final class ResolvedCodeBox: @unchecked Sendable {
        let lock = NSLock()
        var code: AppLocaleCode = .english
    }

    private static let box = ResolvedCodeBox()

    static var resolvedCode: AppLocaleCode {
        box.lock.lock()
        defer { box.lock.unlock() }
        return box.code
    }

    static func setResolvedCode(_ code: AppLocaleCode) {
        box.lock.lock()
        box.code = code
        box.lock.unlock()
    }

    static var effectiveLocale: Locale {
        Locale(identifier: resolvedCode.rawValue)
    }

    static func t(_ key: String) -> String {
        switch resolvedCode {
        case .english:
            return key
        case .simplifiedChinese:
            return localizedString(key, localeCode: .simplifiedChinese)
        }
    }

    static func format(_ key: String, _ args: CVarArg...) -> String {
        String(format: t(key), locale: effectiveLocale, arguments: args)
    }

    static func resolveSystemCode() -> AppLocaleCode {
        for preferred in Locale.preferredLanguages {
            let lower = preferred.lowercased()
            if lower.hasPrefix("zh-hans")
                || lower.hasPrefix("zh-cn")
                || lower == "zh"
                || lower.hasPrefix("zh-sg") {
                return .simplifiedChinese
            }
            if lower.hasPrefix("zh") {
                // zh-Hant / zh-TW / zh-HK: no Traditional table yet → English.
                return .english
            }
            if lower.hasPrefix("en") {
                return .english
            }
        }
        if let code = Locale.current.language.languageCode?.identifier.lowercased(),
           code == "zh" {
            let script = Locale.current.language.script?.identifier.lowercased()
            if script == "hans" { return .simplifiedChinese }
        }
        return .english
    }

    static func resolveCode(for preference: AppLanguage) -> AppLocaleCode {
        switch preference {
        case .english: return .english
        case .simplifiedChinese: return .simplifiedChinese
        case .system: return resolveSystemCode()
        }
    }

    /// Look up `key` in the String Catalog compiled into the module resource
    /// bundle's `zh-Hans.lproj` (or `en.lproj`). Falls back to the English key.
    private static func localizedString(_ key: String, localeCode: AppLocaleCode) -> String {
        guard let root = ModuleResources.bundle else { return key }
        if let path = root.path(forResource: localeCode.lprojName, ofType: "lproj"),
           let langBundle = Bundle(path: path) {
            let value = langBundle.localizedString(forKey: key, value: nil, table: "Localizable")
            // Foundation returns the key itself when the entry is missing.
            if value != key { return value }
            // Some SPM layouts omit the table name; try the default table.
            let fallback = langBundle.localizedString(forKey: key, value: nil, table: nil)
            if fallback != key { return fallback }
        }
        // Last resort: let the root bundle pick from its preferred localizations.
        let fromRoot = root.localizedString(forKey: key, value: nil, table: "Localizable")
        return fromRoot == key
            ? root.localizedString(forKey: key, value: key, table: nil)
            : fromRoot
    }
}

@MainActor
final class LocalizationStore: ObservableObject {
    static let shared = LocalizationStore()

    static let preferenceKey = "unpeel.native.appLanguage"
    static let languageDidChange = Notification.Name("unpeelLanguageDidChange")

    /// Bumped on every preference change so SwiftUI roots can `.id` rebuild.
    @Published private(set) var revision: Int = 0
    @Published private(set) var preference: AppLanguage

    private init() {
        let preference = AppDefaults.shared.string(forKey: Self.preferenceKey)
            .flatMap(AppLanguage.init(rawValue:)) ?? .system
        self.preference = preference
        L10n.setResolvedCode(L10n.resolveCode(for: preference))
    }

    var resolvedCode: AppLocaleCode {
        L10n.resolveCode(for: preference)
    }

    var effectiveLocale: Locale {
        Locale(identifier: resolvedCode.rawValue)
    }

    func setPreference(_ language: AppLanguage) {
        AppDefaults.shared.set(language.rawValue, forKey: Self.preferenceKey)
        L10n.setResolvedCode(L10n.resolveCode(for: language))
        guard language != preference else { return }
        preference = language
        revision &+= 1
        NotificationCenter.default.post(name: Self.languageDidChange, object: nil)
    }
}
