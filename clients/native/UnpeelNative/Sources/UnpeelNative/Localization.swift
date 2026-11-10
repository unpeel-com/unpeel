//
//  Localization.swift
//  UnpeelNative
//
//  App UI language preference (Settings ▸ Appearance ▸ Language) and an
//  in-process string table. Native-only UserDefaults overlay — never written
//  to app-state.json. English source strings are the lookup keys; missing
//  translations fall back to English.
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
            return zhHans[key] ?? key
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

    /// Simplified Chinese UI strings. Keys are the English source text.
    private static let zhHans: [String: String] = [
        // Language / theme
        "System": "跟随系统",
        "Light": "浅色",
        "Dark": "深色",
        "Language": "语言",
        "Language for the Unpeel interface. System follows macOS.":
            "Unpeel 界面语言。跟随系统即使用 macOS 的首选语言。",

        // Settings tabs / chrome
        "Settings": "设置",
        "Settings…": "设置…",
        "Settings — %@": "设置 — %@",
        "Back": "返回",
        "Appearance": "外观",
        "Agents": "智能体",
        "Plugins": "插件",
        "Git": "Git",
        "Agent access": "智能体权限",
        "Remote Control": "远程控制",
        "Workspaces": "工作区",
        "Transcripts": "对话记录",
        "Notifications": "通知",
        "Computer use": "电脑控制",
        "Worktrees": "工作树",
        "Features": "功能",
        "Advanced": "高级",
        "Feedback & bugs": "反馈与缺陷",

        // Appearance panel
        "How Unpeel looks. System follows your macOS appearance.":
            "Unpeel 的外观。跟随系统即与 macOS 外观一致。",
        "Mode": "模式",
        "Applies to the window, sidebar and terminal colors. Claude Code has its own theme setting — run /config inside Claude Code and change Theme to match.":
            "应用于窗口、侧栏和终端颜色。Claude Code 有自己的主题设置 — 在 Claude Code 内运行 /config，将 Theme 改成一致即可。",
        "App color": "应用配色",
        "Washes this workspace's window chrome — sidebar, content, and terminal canvas. Each workspace keeps its own color (also editable per workspace in Settings ▸ Workspaces).":
            "为此工作区的窗口铬层着色 — 侧栏、内容区和终端画布。每个工作区各自保留颜色（也可在 设置 ▸ 工作区 中按工作区编辑）。",
        "Session titles": "会话标题",
        "What names a session in the sidebar until you rename it. First prompt titles it once from your first message. Live from agent follows the agent's own task summary as it works (agents that publish one — Claude today), falling back to the first prompt until it appears. Renaming a session always wins.":
            "侧栏中会话在你重命名之前的显示名称。首次提示用你的第一条消息命名一次。跟随智能体则采用智能体发布的任务摘要（目前 Claude 会发布），在出现前回退到首次提示。手动重命名始终优先。",
        "First prompt": "首次提示",
        "Live from agent": "跟随智能体",
        "Off": "关闭",
        "Transparency": "透明度",
        "Background": "背景",
        "Surface": "表层",
        "Revert to default": "恢复默认",
        "Background is the window backdrop — the sidebar and everything behind the content; below 100% the desktop shows through it, natively blurred. Surface covers the terminal canvas, settings, and the other pages on top of it. 100% is fully opaque. Terminal text always stays fully opaque.":
            "背景是窗口衬底 — 侧栏以及内容背后的一切；低于 100% 时桌面会透过并带系统模糊。表层覆盖其上的终端画布、设置等页面。100% 为完全不透明。终端文字始终保持不透明。",
        "Open resources": "打开资源",
        "Editor": "编辑器",
        "Choose what opens each supported type in this workspace. The editor is also used by \"Open in editor\" and the titlebar open button.":
            "选择此工作区中各受支持类型用什么打开。编辑器也用于「在编辑器中打开」和标题栏打开按钮。",
        "New terminal": "新建终端",
        "Preset screen": "预设选择屏",
        "⌘T": "⌘T",

        // Sidebar / empty states
        "No sessions yet.": "还没有会话。",
        "No active sessions.": "没有进行中的会话。",
        "No active sessions": "没有进行中的会话",
        "New session": "新建会话",
        "Terminal": "终端",
        "Manage Agents…": "管理智能体…",
        "Manage Plugins…": "管理插件…",
        "Folder color": "文件夹颜色",
        "Default": "默认",
        "Sort sessions": "会话排序",
        "Custom order": "自定义顺序",
        "Recently updated": "最近更新",
        "New worktree…": "新建工作树…",
        "New group…": "新建分组…",
        "Pin": "固定",
        "Unpin": "取消固定",
        "Add Project": "添加项目",
        "Add Project…": "添加项目…",
        "Add Workspace…": "添加工作区…",
        "Add project or workspace": "添加项目或工作区",
        "Settings (⌘,)": "设置 (⌘,)",
        "All recent": "全部最近",
        "Back to workspace": "返回工作区",
        "Show sidebar": "显示侧栏",
        "Hide sidebar": "隐藏侧栏",
        "Show sidebar (⌘B)": "显示侧栏 (⌘B)",
        "Hide sidebar (⌘B)": "隐藏侧栏 (⌘B)",
        "Stop?": "停止？",
        "Pinned": "已固定",
        "Search presets…": "搜索预设…",
        "No matching presets": "没有匹配的预设",

        // App / session menus
        "About Unpeel": "关于 Unpeel",
        "Check for Updates…": "检查更新…",
        "Services": "服务",
        "Hide Unpeel": "隐藏 Unpeel",
        "Hide Others": "隐藏其他",
        "Show All": "全部显示",
        "Quit Unpeel": "退出 Unpeel",
        "Session": "会话",
        "New Session": "新建会话",
        "New Terminal": "新建终端",
        "Choose Preset…": "选择预设…",
        "Split Pane Right": "向右分屏",
        "Split Pane Down": "向下分屏",
        "Zoom Pane": "缩放窗格",
        "Equalize Splits": "均分窗格",
        "Focus Pane Left": "聚焦左侧窗格",
        "Focus Pane Right": "聚焦右侧窗格",
        "Focus Pane Up": "聚焦上方窗格",
        "Focus Pane Down": "聚焦下方窗格",
        "Close Pane": "关闭窗格",
        "Close Window": "关闭窗口",
        "Command Palette": "命令面板",
        "Take Screenshot…": "截取屏幕…",
        "Collapse All Folders": "折叠全部文件夹",
        "Edit": "编辑",
        "Undo": "撤销",
        "Redo": "重做",
        "Cut": "剪切",
        "Copy": "拷贝",
        "Paste": "粘贴",
        "Select All": "全选",
        "Find…": "查找…",
        "Find Next": "查找下一个",
        "Find Previous": "查找上一个",
        "View": "显示",
        "Increase Font Size": "增大字号",
        "Decrease Font Size": "减小字号",
        "Reset Font Size": "重置字号",
        "Window": "窗口",
        "Minimize": "最小化",
        "Zoom": "缩放",
        "Bring All to Front": "前置全部窗口",
        "Help": "帮助",
        "Unpeel Help": "Unpeel 帮助",
        "Documentation": "文档",
        "Unpeel sessions": "Unpeel 会话",
        "Unpeel sessions — %@": "Unpeel 会话 — %@",
        "This workspace is already running": "此工作区已在运行",
        "Another Unpeel instance is already using this workspace's data. Switch to it instead.":
            "另一个 Unpeel 实例已在使用此工作区的数据。请切换到该实例。",
        "On This Mac": "本机",
        "This Mac": "本机",
        "Sky": "天蓝",
        "Blue": "蓝色",
        "Violet": "紫色",
        "Rose": "玫红",
        "Amber": "琥珀",
        "Moss": "苔绿",
        "Teal": "青绿",
        "Graphite": "石墨",
    ]
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
