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
        "Every workspace this app can be in — on this Mac and on remote machines. Selecting one scopes the whole app to it; drag to reorder.":
            "此应用可进入的全部工作区 — 本机与远程机器。点选一项会将整个应用切到该工作区；可拖拽排序。",
        "Every workspace on this Mac has its own sessions, projects, presets, and settings. Selecting one scopes this window to it; drag to reorder.":
            "本机上的每个工作区都有各自的会话、项目、预设和设置。点选一项会将本窗口切到该工作区；可拖拽排序。",
        "All workspaces": "全部工作区",
        "Click the color dot to set a workspace's color, or the ⋯ menu to rename or remove it. Drag to reorder — the sidebar switcher matches. Add Workspace… covers this Mac, pairing, and SSH.":
            "点击色点设置工作区颜色，或用 ⋯ 菜单重命名/移除。拖拽排序会与侧栏切换器一致。「添加工作区…」覆盖本机、配对和 SSH。",
        "Click a workspace to select it. Set each one's color with its dot, use the ⋯ menu to rename or remove it, and drag to reorder — the sidebar switcher matches.":
            "点击工作区即可选中。用色点设置颜色，用 ⋯ 菜单重命名或移除，拖拽排序会与侧栏切换器一致。",
        "New Workspace…": "新建工作区…",
        "Rename workspace": "重命名工作区",
        "Name": "名称",
        "Shows in the menu bar and on paired phones. The phone name fully updates after the workspace restarts.":
            "显示在菜单栏和已配对的手机上。手机上的名称会在工作区重启后完全更新。",
        "workspace": "工作区",
        "Remove %@?": "移除 %@？",
        "Remove and delete its data": "移除并删除其数据",
        "\"Remove from list\" keeps %1$@'s data at %2$@ so you can re-add it later. Deleting its data also unpairs any phones paired with it.":
            "「从列表移除」会保留 %1$@ 在 %2$@ 的数据，之后还可重新添加。删除数据同时会解除与其配对的手机。",
        "Share…": "共享…",
        "Open %1$@'s Remote Control settings": "打开 %1$@ 的远程控制设置",
        "Set %1$@'s color": "设置 %1$@ 的颜色",
        "Remove…": "移除…",
        "Quit %@ before removing it.": "请先退出 %@，再移除它。",
        "Add iPhone or iPad…": "添加 iPhone 或 iPad…",
        "Forget Workspace": "忘记工作区",
        "Extra workspaces require Unpeel Link": "额外工作区需要 Unpeel Link",
        "Unpeel Link adds workspaces, remote control from your iPhone, and Unpeel Remote access from anywhere — $59 per seat per year.":
            "Unpeel Link 提供工作区、iPhone 远程控制，以及随时随地的 Unpeel Remote 访问 — 每席位每年 $59。",
        "Open Unpeel Link Settings": "打开 Unpeel Link 设置",
        "App color for %@": "%@ 的应用配色",
        "Washes %1$@'s window chrome and shows on its paired phones. Any other workspace's color is set from its row's dot in the list above.":
            "为 %1$@ 的窗口铬层着色，并显示在其已配对的手机上。其他工作区的颜色可在上方列表对应行的色点设置。",
        "Not shared": "未共享",
        "1 device": "1 台设备",
        "%d devices": "%d 台设备",
        "%1$@ · Link %2$@": "%1$@ · Link %2$@",
        "on": "开",
        "off": "关",
        "This instance": "当前实例",
        "Running": "运行中",
        "Remote": "远程",
        "SSH · %@": "SSH · %@",
        "Personal": "个人",
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
        "Rename": "重命名",
        "Rename…": "重命名…",
        "Move to": "移动到",
        "Workspace": "工作区",
        "Stop all": "全部停止",
        "Reveal in Finder": "在 Finder 中显示",
        "Open in": "打开方式",
        "Remove worktree": "移除工作树",
        "Remove group": "移除分组",
        "Remove": "移除",
        "Cancel": "取消",
        "Pin to global project sidebar": "固定到全局项目侧栏",
        "Unpin from global project sidebar": "从全局项目侧栏取消固定",
        "Clear attention": "清除需关注",
        "Resume Agent": "恢复智能体",
        "Resume": "恢复",
        "Restart App": "重启应用",
        "Notify when done": "完成时通知",
        "Copy session ID": "拷贝会话 ID",
        "Stop": "停止",
        "Remove session": "移除会话",
        "Remove from list": "从列表移除",
        "Remove session?": "移除会话？",
        "Remove from list?": "从列表移除？",
        "Copy transcript": "拷贝对话记录",
        "Last 20 entries": "最近 20 条",
        "Last 50 entries": "最近 50 条",
        "Last 100 entries": "最近 100 条",
        "Whole conversation": "完整对话",
        "Read": "读取",
        "Write": "写入",
        "Create & remove": "创建与移除",
        "Reads any session; writes follow Settings.": "可读取任意会话；写入遵循设置。",
        "Also writes to other sessions.": "还可写入其他会话。",
        "Also starts and removes its own sessions.": "还可创建并移除自己的会话。",
        "What this agent may do to other sessions through Unpeel's tools":
            "此智能体通过 Unpeel 工具可对其他会话执行的操作",
        "Copy the conversation as Markdown, using your Settings ▸ Transcripts content options":
            "按「设置 ▸ 对话记录」的内容选项，以 Markdown 拷贝对话",
        "Detach Pane": "分离窗格",
        "Exit Multi-Pane View": "退出多窗格视图",
        "Open in New Window": "在新窗口中打开",
        "Manage Workspaces…": "管理工作区…",
        "Capture area": "截取区域",
        "Capture window": "截取窗口",
        "Capture full screen": "截取全屏",
        "Add to prompt": "添加到提示",
        "Open": "打开",
        "Delete": "删除",
        "More…": "更多…",
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
        "this Host":
            "此 Host",
        "this workspace":
            "此工作区",
        "this Mac":
            "此 Mac",
        "%@ cannot mint pairing invitations over this connection. Pair devices from its own running Unpeel instead — the terminal UI's Settings ▸ Remote, or `unpeel pair`.":
            "%@ 无法通过此连接生成配对邀请。请在其运行的 Unpeel 中配对 — 终端 UI「设置 ▸ 远程」，或 `unpeel pair`。",
        "These devices reach %@ — and these Hosts stay reachable — from any network, through the unpeel.com relay. Session traffic is end-to-end encrypted; notification titles pass through Unpeel and Apple Push. Everything not listed here connects direct-only, on your own network.":
            "这些设备可从任意网络经 unpeel.com 中继访问 %@；所列 Host 也可达。会话流量端到端加密；通知标题经 Unpeel 与 Apple Push。未列出的连接仅在自有网络直连。",
        "Update Unpeel on %@, then reconnect this workspace. Its settings stay unchanged until the Host advertises the required capability.":
            "请更新 %@ 上的 Unpeel，然后重新连接此工作区。在 Host 广播所需能力之前，其设置保持不变。",
        "What names new sessions on the Host until you rename them. Running session hosts read this live.":
            "在重命名前 Host 上新会话的显示名称。运行中的会话宿主会实时读取。",
        "%@'s cleanup behavior, applied over the Host connection — the Host's own auto-stop sweep uses these; the app never stops sessions on its own. Memory diagnostics are per-instance and live in each workspace's own window.":
            "%@ 的清理行为，经 Host 连接应用 — Host 的自动停止扫描使用这些设置；应用不会自行停止会话。内存诊断按实例进行，位于各工作区自有窗口。",
        "%1$@'s %2$@ policy, applied over the Host connection — its sessions read these per call, so changes apply live. Engine status, approvals, and site rules live in %1$@'s own window for now.":
            "%1$@ 的 %2$@ 策略，经 Host 连接应用 — 其会话按调用读取，更改即时生效。引擎状态、批准与站点规则暂在 %1$@ 自有窗口。",
        "How %1$@ flags sessions that need you. Applies to %2$@'s own instance — live if it is running, otherwise from its next launch.":
            "%1$@ 如何标记需你处理的会话。应用于 %2$@ 自有实例 — 运行中即时生效，否则下次启动生效。",
        "%1$@ uses %2$@'s notification settings until a setting below is changed. Revert drops %1$@'s own values.":
            "%1$@ 在更改下方设置前使用 %2$@ 的通知设置。恢复默认会清除 %1$@ 自有值。",
        "%@'s optional features. Some changes fully apply when %@ next launches.":
            "%@ 的可选功能。部分更改在 %@ 下次启动后完全生效。",
        "%1$@ uses %2$@'s features until a toggle below is changed. Revert drops %1$@'s own values.":
            "%1$@ 在更改下方开关前使用 %2$@ 的功能。恢复默认会清除 %1$@ 自有值。",
        "Start with the session's title, ID, CLI, and model. The ID lets another agent target this session with the Sessions MCP tools.":
            "以会话标题、ID、CLI 和模型开头。ID 可供另一智能体通过 Sessions MCP 工具定位此会话。",
        "%d running · %@ CPU · %@ memory. Sorted by current CPU usage.":
            "%d 个运行中 · %@ CPU · %@ 内存。按当前 CPU 排序。",
        "A macOS banner (and a push to a paired iPhone) when a session needs input, or finishes if you turned on “Notify when done” for it. Phone alerts use Link/APNs even while terminal traffic stays Direct or SSH. Mac and phone tests exercise their respective delivery paths; phone diagnostics distinguish a missing APNs token, Link entitlement failure, and APNs rejection.":
            "会话需要输入时显示 macOS 横幅（并向已配对 iPhone 推送）；若开启「完成时通知」也会在完成时通知。手机提醒经 Link/APNs，终端流量仍可走直连或 SSH。",

        // Settings ▸ Git
        "GitHub access and Git identity on %@.":
            "%@ 上的 GitHub 访问与 Git 身份。",
        "These settings use this Host’s user account. Workspaces running under the same user share Git identity and GitHub sign-in.":
            "这些设置使用该 Host 的用户账户。在同一用户下运行的工作区共享 Git 身份与 GitHub 登录。",
        "Refresh status": "刷新状态",
        "Checking Git on this Host…": "正在检查此 Host 上的 Git…",
        "Connect to this workspace and update Unpeel on its Host to use Git settings.":
            "连接此工作区并在其 Host 上更新 Unpeel 后，方可使用 Git 设置。",
        "Install GitHub CLI on %@": "在 %@ 上安装 GitHub CLI",
        "Check installation": "检查安装",
        "Hide setup terminal": "隐藏安装终端",
        "GitHub CLI setup": "GitHub CLI 安装",
        "Disconnect GitHub on %@?": "在 %@ 上断开 GitHub？",
        "Disconnect @%@": "断开 @%@",
        "Removes this account from GitHub CLI on this Host. Other workspaces using the same server user are affected. It does not revoke the authorization on GitHub.":
            "将从该 Host 的 GitHub CLI 移除此账户。使用同一服务器用户的其他工作区会受影响。不会撤销 GitHub 上的授权。",
        "GitHub": "GitHub",
        "GitHub CLI is not installed on this Host.": "此 Host 未安装 GitHub CLI。",
        "Install GitHub CLI…": "安装 GitHub CLI…",
        "Add a folder to this workspace to open a setup terminal.":
            "向此工作区添加文件夹以打开安装终端。",
        "GitHub CLI installation instructions": "GitHub CLI 安装说明",
        "Connected as @%@": "已连接为 @%@",
        "GitHub connection needs attention": "GitHub 连接需要处理",
        "Not connected": "未连接",
        "Authentication comes from an environment token on this Host.":
            "认证来自此 Host 上的环境变量令牌。",
        "Disconnect…": "断开…",
        "Sign in with GitHub": "使用 GitHub 登录",
        "GitHub credentials stay on this Host. This sign-in flow does not send them to Unpeel’s service.":
            "GitHub 凭据保留在此 Host。此登录流程不会将它们发送到 Unpeel 服务。",
        "Stored in this Host’s system credential store.":
            "存储在此 Host 的系统凭据库中。",
        "GitHub CLI is storing the token in a plain-text file on this Host. Configure a system credential store and sign in again to use it.":
            "GitHub CLI 正将此 Host 上的令牌存放在纯文本文件中。请配置系统凭据库并重新登录以使用它。",
        "This GitHub CLI version does not report credential storage. Check it on the Host with gh auth status.":
            "此 GitHub CLI 版本不报告凭据存储方式。请在 Host 上用 gh auth status 检查。",
        "Git HTTPS access configured": "已配置 Git HTTPS 访问",
        "Configure Git access": "配置 Git 访问",
        "Paste this code into GitHub in your browser to authorize %@.":
            "在浏览器中将此代码粘贴到 GitHub 以授权 %@。",
        "Copy Code and Open GitHub": "复制代码并打开 GitHub",
        "Starting GitHub sign-in…": "正在启动 GitHub 登录…",
        "Cancel sign-in": "取消登录",
        "Git identity": "Git 身份",
        "Email": "电子邮件",
        "Used for new commits. Repository-specific Git configuration can override these defaults.":
            "用于新提交。仓库特定的 Git 配置可覆盖这些默认值。",
        "Save identity": "保存身份",
        "Git is not installed on this Host.": "此 Host 未安装 Git。",
        "Could not open GitHub. Open github.com/login/device and enter the code above.":
            "无法打开 GitHub。请打开 github.com/login/device 并输入上方代码。",

        // Settings ▸ Worktrees
        "Isolated git checkouts so several agents can work the same repo without touching each other's files. Create one from a git project, or show the worktrees agents make for themselves.":
            "独立的 git 检出，让多个智能体可在同一仓库上工作而互不干扰。可从 git 项目创建，或显示智能体自建的工作树。",
        "Show agent worktrees": "显示智能体工作树",
        "Show the working copies agents create for themselves (Claude Code and similar) as folders under their project in the sidebar.":
            "在侧栏中将智能体（Claude Code 等）自建的工作副本显示为其项目下的文件夹。",
        "Agent worktrees": "智能体工作树",
        "Some agents make their own git worktree of a project to work in. Turn this on to see those worktrees in the sidebar; turning it off hides them again without deleting anything.":
            "部分智能体会为项目创建自己的 git 工作树。开启后可在侧栏看到；关闭仅隐藏，不会删除。",
        "Let sessions create worktrees": "允许会话创建工作树",
        "Agents can prepare isolated git worktrees as child projects in the sidebar. Launching sessions into them is still up to you. Applies immediately.":
            "智能体可在侧栏中准备独立的 git 工作树作为子项目。是否在其中启动会话仍由你决定。立即生效。",
        "Session access": "会话权限",
        "Whether sessions may prepare worktrees through Unpeel MCP. They still cannot launch sessions themselves.":
            "会话是否可通过 Unpeel MCP 准备工作树。它们仍不能自行启动会话。",
        "This workspace": "此工作区",
        "Worktrees registered under this workspace's projects.":
            "此工作区项目下注册的工作树。",
        "Each worktree is a child folder of its project in the sidebar.":
            "每个工作树在侧栏中是其项目的子文件夹。",
        "No worktrees in this workspace. Create one from a git project, or turn on Show agent worktrees to adopt copies agents make for themselves.":
            "此工作区没有工作树。可从 git 项目创建，或开启「显示智能体工作树」以采纳智能体自建的副本。",
        "No worktrees in this workspace.": "此工作区没有工作树。",
        "Agent": "智能体",
        "1 session": "1 个会话",
        "%d sessions": "%d 个会话",
        "Project": "项目",

        // Settings ▸ Unpeel Link (license sections)
        "Your name": "你的名称",
        "Display name": "显示名称",
        "Avatar %@": "头像 %@",
        "Avatar": "头像",
        "Presence": "在线状态",
        "How you appear to others connected to the same Host — the name and avatar on presence chips.":
            "你在同一 Host 上向他人展示的方式 — 在线状态芯片上的名称与头像。",
        "Unpeel Link is active": "Unpeel Link 已激活",
        "Licensed to": "许可给",
        "Seats": "席位",
        "%d Hosts": "%d 个 Host",
        "Deactivating…": "正在停用…",
        "Deactivate this Host": "停用此 Host",
        "Release seat": "释放席位",
        "Unpeel Link subscription": "Unpeel Link 订阅",
        "One compatibility seat activates one Host. Deactivating frees this Host's seat so you can use the key on a different machine.":
            "一个兼容席位激活一个 Host。停用可释放此 Host 的席位，以便在其他机器上使用密钥。",
        "This Unpeel Link key is no longer active": "此 Unpeel Link 密钥已不再有效",
        "The subscription for %@ has ended or been refunded. Unpeel keeps working — renew or enter a different key to use Unpeel Link again.":
            "%@ 的订阅已结束或已退款。Unpeel 仍可继续使用 — 续订或输入其他密钥以再次使用 Unpeel Link。",
        "Unpeel Remote": "Unpeel Remote",
        "Reach your Macs from anywhere over an end-to-end encrypted relay.":
            "通过端到端加密中继从任何地方访问你的 Mac。",
        "iPhone remote control": "iPhone 远程控制",
        "Pair the Unpeel iOS app to steer sessions from your phone.":
            "配对 Unpeel iOS 应用，从手机操控会话。",
        "Run separate Unpeel instances with their own sessions and settings.":
            "运行独立的 Unpeel 实例，各有自己的会话与设置。",
        "Get Unpeel Link": "获取 Unpeel Link",
        "Unpeel is free on your Mac; Link is the relay above — $59 per seat per year, one compatibility seat activates one Host. Paste the key from your purchase email to activate.":
            "Unpeel 在 Mac 上免费；Link 是上方所述中继 — 每席位每年 $59，一个兼容席位激活一个 Host。粘贴购买邮件中的密钥即可激活。",
        "Lost your key?": "丢失密钥？",
        "Activating…": "正在激活…",
        "Activate": "激活",

        // Add workspace / remote pairing sheets
        "Expires in %d:%02d": "于 %d:%02d 后过期",
        "Add an iPhone or iPad to %@": "向 %@ 添加 iPhone 或 iPad",
        "This Mac forwards a one-time sealed exchange to the remote workspace.":
            "本机会将一次性密封交换转发到远程工作区。",
        "Done": "完成",
        "Device added to %@": "设备已添加到 %@",
        "The phone now has its own revocable Direct and Link credentials for %@.":
            "手机现已拥有针对 %@ 的可撤销 Direct 与 Link 凭据。",
        "Add Another iPhone or iPad": "再添加一台 iPhone 或 iPad",
        "Scan this code in Unpeel on the phone.":
            "在手机的 Unpeel 中扫描此码。",
        "The phone only needs to reach this Mac for the one-time sealed exchange. The remote workspace creates the credentials; this Mac forwards the sealed response without opening it.":
            "手机只需在一次性密封交换期间连到本机。远程工作区创建凭据；本机转发密封响应而不打开。",
        "Creating invitation…": "正在创建邀请…",
        "Generate QR Code": "生成二维码",
        "Refresh QR Code": "刷新二维码",
        "Copy Pairing Code": "复制配对码",
        "After pairing, the phone connects to %@ itself—Direct when reachable, otherwise through Unpeel Link if enabled.":
            "配对后，手机自行连接 %@ — 可达时用 Direct，否则在启用时经 Unpeel Link。",
        "Nearby or code": "附近或配对码",
        "SSH": "SSH",
        "Add Workspace": "添加工作区",
        "New Workspace": "新建工作区",
        "A new workspace on this Mac, or one on another machine you control.":
            "在本机新建工作区，或在你控制的另一台机器上。",
        "A new, separate workspace on this Mac.":
            "在本机新建一个独立工作区。",
        "Kind": "类型",
        "Workspace name": "工作区名称",
        "Work": "工作",
        "Starts blank, fully separate from this workspace: its own sessions, projects, presets, and settings, and it pairs with your phone as its own Mac.":
            "从空白开始，与此工作区完全分离：自有会话、项目、预设和设置，并以独立 Mac 与手机配对。",
        "Color": "颜色",
        "Created workspaces appear in the workspace switcher; renaming never moves their data.":
            "创建的工作区会出现在工作区切换器中；重命名不会移动其数据。",
        "Create": "创建",
        "Nearby": "附近",
        "Nearby discovery is unavailable": "附近发现不可用",
        "You can still paste the pairing code below.":
            "你仍可在下方粘贴配对码。",
        "Workspaces on your network appear here. On the other machine, run `unpeel serve` (or open Unpeel).":
            "网络上的工作区会显示在此。在另一台机器上运行 `unpeel serve`（或打开 Unpeel）。",
        "Use pairing code only": "仅使用配对码",
        "Pairing code": "配对码",
        "On the other machine, run `unpeel pair` — or `unpeel pair --serve` to start serving at the same time — then enter the code it shows here. For another Mac running the Unpeel app, its code is in Settings ▸ Remote Control.":
            "在另一台机器上运行 `unpeel pair` — 或 `unpeel pair --serve` 同时开始服务 — 然后在此输入显示的码。运行 Unpeel 应用的另一台 Mac，其码在 设置 ▸ 远程控制。",
        "Codes work once and expire after 5 minutes.":
            "配对码仅用一次，5 分钟后过期。",
        "Pair": "配对",
        "SSH destination": "SSH 目标",
        "user@host or SSH config alias": "user@host 或 SSH 配置别名",
        "Unpeel uses your system SSH configuration, keys, agent, host-key policy, ProxyJump, and VPN. Put ports and identity files in ~/.ssh/config.":
            "Unpeel 使用系统 SSH 配置、密钥、代理、主机密钥策略、ProxyJump 和 VPN。端口与身份文件请写在 ~/.ssh/config。",
        "Password or API key (optional)": "密码或 API 密钥（可选）",
        "Leave empty for SSH keys or agent": "使用 SSH 密钥或代理时留空",
        "Saved in this Mac’s Keychain so Unpeel can reopen this workspace. It is never stored on the remote machine or passed in command arguments.":
            "保存在本机钥匙串，以便 Unpeel 重新打开此工作区。不会存于远程机器或传入命令参数。",
        "On the remote machine": "在远程机器上",
        "Unpeel could not be started. Install it with the same SSH connection, or copy the command as a fallback:":
            "无法启动 Unpeel。请用同一 SSH 连接安装，或复制以下命令作为备用：",
        "If Unpeel is missing, Connect will offer to install it. Manual fallback:":
            "若未安装 Unpeel，连接时会提示安装。手动备用：",
        "Install & Connect": "安装并连接",
        "Connect": "连接",
        "Supports ordinary SSH and interactive-only managed shells automatically.\nNo service needs to run on the box — Unpeel connects over SSH on demand. For phone access and notifications while disconnected, also run `unpeel serve` there.":
            "自动支持普通 SSH 与仅交互的管理 shell。\n机器上无需常驻服务 — Unpeel 按需经 SSH 连接。若要在断开时用手机访问和通知，也请在那里运行 `unpeel serve`。",
        "Unpeel is not available on this SSH Host yet, or could not be started. Install it and connect again.":
            "此 SSH Host 上尚无 Unpeel，或无法启动。请安装后重新连接。",

        // Settings ▸ Agents, Plugins, Agent access
        "%@ has no hooks or MCP configuration Unpeel can register; Unpeel still recognizes it by name.":
            "%@ 没有 Unpeel 可注册的钩子或 MCP 配置；Unpeel 仍可通过名称识别它。",
        "%@ is installed without its Unpeel integration":
            "%@ 已安装但未配置 Unpeel 集成",
        "%@ — run `unpeel browser install` on the Host to retry (it re-verifies the pinned agent-browser %@ by sha256).":
            "%@ — 在 Host 上运行 `unpeel browser install` 重试（会按 sha256 重新校验固定的 agent-browser %@）。",
        "%d agents are installed without their Unpeel integration":
            "%d 个智能体已安装但未配置 Unpeel 集成",
        "A pointer glides to whatever the agent clicks or fills, so you can follow along. Adds a short beat before each action.":
            "指针会滑向智能体点击或填写处，便于你跟随。每次操作前会有短暂停顿。",
        "Actions happen instantly, with no visible pointer.":
            "操作即时完成，不显示指针。",
        "Activate %@":
            "激活 %@",
        "Active":
            "活跃",
        "Add":
            "添加",
        "Add %@ to Quick Launch":
            "将 %@ 添加到快速启动",
        "Add another command below":
            "在下方添加另一条命令",
        "Add browser screenshots automatically":
            "自动添加浏览器截图",
        "Add custom command…":
            "添加自定义命令…",
        "Add to Quick Launch":
            "添加到快速启动",
        "Agents can drive a real browser — open pages, click, fill forms, and take screenshots. Sessions in a project share one browser window and logins, each agent pinned to its own tab.":
            "智能体可操控真实浏览器 — 打开页面、点击、填表、截图。同一项目中的会话共享一个浏览器窗口与登录态，每个智能体固定在自己的标签页。",
        "All sessions can control this Mac without asking.":
            "所有会话均可不经询问控制此 Mac。",
        "All sites allowed. Add comma-separated domains to restrict browsing — wildcards like *.example.com work, and the browser itself blocks everything else (pages, scripts, requests). Press Return to apply.":
            "允许所有网站。添加以逗号分隔的域名可限制浏览 — 支持 *.example.com 等通配符，浏览器会拦截其余页面、脚本与请求。按回车应用。",
        "Allow":
            "允许",
        "Always allow":
            "始终允许",
        "Any session can write to any other session without asking.":
            "任意会话均可不经询问写入其他会话。",
        "App-panel launch approvals you've granted to agent sessions.":
            "你已授予智能体会话的 App 面板启动批准。",
        "Applied to the agent's next browser action — no restart needed.":
            "应用于智能体下一次浏览器操作 — 无需重启。",
        "Approved Apps":
            "已批准的 App",
        "Approved browser sessions":
            "已批准的浏览器会话",
        "Approved sessions":
            "已批准的会话",
        "Ask each session":
            "逐会话询问",
        "Ask for approval":
            "请求批准",
        "Auto-detect Chrome":
            "自动检测 Chrome",
        "Available to install":
            "可安装",
        "Available version":
            "可用版本",
        "Browser":
            "浏览器",
        "Browser MCP screenshots appear in the current session's gallery.":
            "Browser MCP 截图会出现在当前会话的画廊中。",
        "Browser access":
            "浏览器权限",
        "Browser app":
            "浏览器应用",
        "Browser captures stay out of the gallery until an agent adds a selected image with Sessions use. Explicit phone screenshot requests are still added.":
            "浏览器截图不会进入画廊，直至智能体通过 Sessions 工具添加所选图片。明确的手机截图请求仍会添加。",
        "Browser engine install disabled":
            "浏览器引擎安装已禁用",
        "Browser engine install failed":
            "浏览器引擎安装失败",
        "Browser engine installing…":
            "正在安装浏览器引擎…",
        "Browser engine ready":
            "浏览器引擎已就绪",
        "Browser engine status unknown":
            "浏览器引擎状态未知",
        "Browser scope":
            "浏览器范围",
        "Browsing is restricted to the listed domains, enforced inside the browser engine. Clear the field and press Return to allow all sites.":
            "浏览限于所列域名，由浏览器引擎内部强制执行。清空字段并按回车可允许所有网站。",
        "Can start this App as a companion panel without asking.":
            "可不经询问以此 App 启动伴生面板。",
        "Can type into the session without asking.":
            "可不经询问向该会话输入。",
        "Cancel new command":
            "取消新命令",
        "Choose whether Browser MCP captures are published as they are taken.":
            "选择 Browser MCP 截图是否在拍摄时即发布。",
        "Choose which plugin opens each file type under Appearance ▸ Open resources.":
            "在「外观 ▸ 打开资源」中选择各文件类型由哪个插件打开。",
        "Clear project browser data":
            "清除项目浏览器数据",
        "Clear…":
            "清除…",
        "Connecting to workspace…":
            "正在连接工作区…",
        "Deletes saved project profiles (logins, cookies). Open project windows keep theirs until every session tab closes.":
            "删除已保存的项目配置（登录、Cookie）。已打开的 project 窗口会保留直至所有会话标签关闭。",
        "Dismiss":
            "关闭",
        "Each session gets a separate browser; its cookies and logins vanish when that browser closes.":
            "每个会话有独立浏览器；关闭后其 Cookie 与登录态消失。",
        "Each session's first browser action asks you once.":
            "每个会话的首次浏览器操作会询问你一次。",
        "Each session's first computer action asks you once.":
            "每个会话的首次电脑操作会询问你一次。",
        "Every integration points the agent's MCP config at one launcher on this Host. Register it with an agent's own command (in the agent's details above) to get Unpeel's tools without the hooks; Install integration does both.":
            "每个集成都将智能体的 MCP 配置指向此 Host 上的一个启动器。用智能体自有命令注册（见上方智能体详情）可获 Unpeel 工具而无钩子；「安装集成」二者兼备。",
        "Every session can drive its own pinned browser tab.":
            "每个会话可操控自己固定的浏览器标签页。",
        "Every session can read every other session. Writing to another session follows the setting below — by default Unpeel asks you the first time and remembers your answer per pair. Sidebar groups are organizational only, and agents never create or close sessions unless you right-click one and set its Agent access to Create & remove.":
            "每个会话都可读取其他任意会话。写入其他会话遵循下方设置 — 默认首次询问并按配对记住你的选择。侧栏分组仅作组织用；智能体不会创建或关闭会话，除非你右键某会话并将其智能体权限设为「创建与移除」。",
        "Exact busy and idle from hooks, done notifications, transcripts, precise resume, and Unpeel's tools inside %@.":
            "通过钩子精确获知忙碌/空闲、完成通知、对话记录、精确恢复，以及 %@ 内的 Unpeel 工具。",
        "Gallery":
            "画廊",
        "Global":
            "全局",
        "Hide %@ details":
            "隐藏 %@ 详情",
        "Hide details":
            "隐藏详情",
        "Hide installation terminal":
            "隐藏安装终端",
        "Hide terminal — the session stays in your workspace":
            "隐藏终端 — 会话仍留在工作区中",
        "How agent sessions can see and control each other. Reads are open; writes follow the policy below.":
            "智能体会话如何查看与控制彼此。读取开放；写入遵循下方策略。",
        "Identity and tint from detection; Resume uses %@'s own continue-last.":
            "通过检测识别身份与色调；恢复使用 %@ 自带的 continue-last。",
        "Identity and tint from detection; busy and idle from the screen; Resume uses %@'s own continue-last.":
            "通过检测识别身份与色调；忙碌/空闲来自屏幕；恢复使用 %@ 自带的 continue-last。",
        "Inactive":
            "未活跃",
        "Install":
            "安装",
        "Install %@":
            "安装 %@",
        "Install agent CLIs on this Host, install their Unpeel integrations, and choose what each one launches.":
            "在此 Host 上安装智能体 CLI、安装其 Unpeel 集成，并选择各自的启动项。",
        "Install all":
            "全部安装",
        "Install integration":
            "安装集成",
        "Install it for exact busy and idle, done notifications, transcripts, precise resume, and Unpeel's tools inside each agent. Each integration registers Unpeel's hooks and MCP server in that agent's own configuration once.":
            "安装后可精确获知忙碌/空闲、完成通知、对话记录、精确恢复及每个智能体内的 Unpeel 工具。每次集成会在该智能体配置中注册 Unpeel 钩子与 MCP 服务器一次。",
        "Installation terminal":
            "安装终端",
        "Installed":
            "已安装",
        "Installed version":
            "已安装版本",
        "Installed: %@":
            "已安装：%@",
        "Integration installed":
            "集成已安装",
        "Integration installed: Unpeel's hooks and MCP server are registered in %@'s own config.":
            "集成已安装：Unpeel 的钩子与 MCP 服务器已注册到 %@ 自有配置。",
        "Integration, capabilities, and manual setup":
            "集成、能力与手动设置",
        "Launch command":
            "启动命令",
        "Limit which websites agents can reach.":
            "限制智能体可访问的网站。",
        "MCP only, by hand (no hooks):":
            "仅 MCP，手动（无钩子）：",
        "Make Default":
            "设为默认",
        "Manages all sessions":
            "管理所有会话",
        "Manages sessions in this project":
            "管理此项目中的会话",
        "Manual setup":
            "手动设置",
        "Move down":
            "下移",
        "Move up":
            "上移",
        "Never allow":
            "从不允许",
        "No Unpeel integration":
            "无 Unpeel 集成",
        "No agents match your search.":
            "没有匹配的智能体。",
        "No approved Apps. An agent asks before it starts an App panel for the first time.":
            "尚无已批准的 App。智能体首次启动 App 面板前会询问。",
        "No approved sessions. When you allow a write from the approval dialog, the pair appears here.":
            "尚无已批准的会话。在批准对话框中允许写入后，配对会显示在此。",
        "No browser tools for any session.":
            "任何会话均无浏览器工具。",
        "No computer control for any session.":
            "任何会话均无电脑控制。",
        "No plugins match your search.":
            "没有匹配的插件。",
        "Not Installed":
            "未安装",
        "Off applies immediately to running sessions. Enabling reaches an agent when it starts in a newly configured terminal.":
            "关闭立即作用于运行中的会话。开启后，智能体在新配置的终端中启动时才会收到。",
        "One window per project; every session gets its own tab and all share cookies and logins. Other projects stay separate.":
            "每个项目一个窗口；每个会话自有标签页，共享 Cookie 与登录。其他项目彼此隔离。",
        "Open resources…":
            "打开资源…",
        "Options":
            "选项",
        "Or from any terminal: unpeel integrations install <agent>":
            "或在任意终端：unpeel integrations install <agent>",
        "Overview":
            "概览",
        "Path to a Chromium-based browser (Chrome, Brave, Edge, Arc). Leave empty to auto-detect. Press Return to apply.":
            "基于 Chromium 的浏览器路径（Chrome、Brave、Edge、Arc）。留空则自动检测。按回车应用。",
        "Refresh installed %@":
            "刷新已安装的 %@",
        "Register Unpeel's hooks and MCP server in %@'s own config":
            "在 %@ 自有配置中注册 Unpeel 钩子与 MCP 服务器",
        "Registers Unpeel's hooks and the unpeel MCP server in %@'s own configuration.":
            "在 %@ 自有配置中注册 Unpeel 钩子与 unpeel MCP 服务器。",
        "Reinstall":
            "重新安装",
        "Remove %@ from Quick Launch":
            "从快速启动移除 %@",
        "Remove Command":
            "移除命令",
        "Remove from Quick Launch":
            "从快速启动移除",
        "Revoke":
            "撤销",
        "Rewrite the hook script and MCP registration for this Host build":
            "为此 Host 构建重写钩子脚本与 MCP 注册",
        "Run the installer on this Host":
            "在此 Host 上运行安装程序",
        "Search agents":
            "搜索智能体",
        "Search plugins":
            "搜索插件",
        "Separate per session":
            "按会话分离",
        "Sessions":
            "会话",
        "Sessions cannot write to other sessions.":
            "会话无法写入其他会话。",
        "Shared project window":
            "共享项目窗口",
        "Show":
            "显示",
        "Show %@ details":
            "显示 %@ 详情",
        "Show agent cursor":
            "显示智能体光标",
        "Show an approval dialog the first time a session writes to one of your other sessions. Approvals are remembered per pair.":
            "会话首次写入你的其他会话时显示批准对话框。按配对记住批准结果。",
        "Show browser window":
            "显示浏览器窗口",
        "Site access":
            "网站访问",
        "Starting terminal on this Host…":
            "正在此 Host 上启动终端…",
        "The Host is downloading and verifying agent-browser %@. Browser tools become available as soon as it finishes.":
            "Host 正在下载并校验 agent-browser %@。完成后浏览器工具即可用。",
        "The Host service has not published an engine state yet (no serve.json). Once it is running it installs the engine on its own; `unpeel browser install` does the same by hand.":
            "Host 服务尚未发布引擎状态（无 serve.json）。运行后会自行安装引擎；也可手动运行 `unpeel browser install`。",
        "The browser runs in the background — screenshots still work.":
            "浏览器在后台运行 — 截图仍可用。",
        "This Host does not support installing integrations":
            "此 Host 不支持安装集成",
        "This Host runs with UNPEEL_BROWSER_ENGINE_INSTALL=0, so it never installs the engine itself. Run `unpeel browser install` on the Host, or set UNPEEL_AGENT_BROWSER_BIN to an engine binary.":
            "此 Host 设置了 UNPEEL_BROWSER_ENGINE_INSTALL=0，不会自行安装引擎。在 Host 上运行 `unpeel browser install`，或设置 UNPEEL_AGENT_BROWSER_BIN 为引擎二进制路径。",
        "Turn on Sessions use or Browser use under Features to configure agent access.":
            "在「功能」中开启 Sessions 使用或 Browser 使用以配置智能体权限。",
        "Unpeel Apps that open files and resources in a pane beside your agents. Install and update them here; develop your own with `unpeel apps link`.":
            "在智能体旁窗格中打开文件与资源的 Unpeel App。在此安装与更新；用 `unpeel apps link` 开发你自己的。",
        "Unpeel integration installed":
            "Unpeel 集成已安装",
        "Unpeel integration not installed":
            "Unpeel 集成未安装",
        "Update":
            "更新",
        "Update %@":
            "更新 %@",
        "Update %@ to %@":
            "将 %@ 更新到 %@",
        "Update Unpeel on this Host to reorder %@.":
            "在此 Host 上更新 Unpeel 以重新排序 %@。",
        "View installation instructions for %@":
            "查看 %@ 的安装说明",
        "What connected agents may do through Unpeel's tools: read and type into your other sessions, drive a real browser, prepare worktrees, and publish to the gallery. Reads are open; every write follows the policies here. This is a cooperative policy for agents using Unpeel's tools, not a sandbox.":
            "已连接智能体通过 Unpeel 工具可执行的操作：读取并向其他会话输入、操控真实浏览器、准备工作树、发布到画廊。读取开放；每次写入遵循此处策略。这是使用 Unpeel 工具的智能体协作策略，而非沙箱。",
        "What happens when a session writes to another session.":
            "会话写入另一会话时的行为。",
        "Without it: busy and idle are read from the screen; no done notifications, transcripts, precise resume, or Unpeel tools.":
            "无集成时：忙碌/空闲来自屏幕；无完成通知、对话记录、精确恢复或 Unpeel 工具。",
        "Without it: identity and tint only; no busy and idle, done notifications, transcripts, precise resume, or Unpeel tools.":
            "无集成时：仅有身份与色调；无忙碌/空闲、完成通知、对话记录、精确恢复或 Unpeel 工具。",
        "Write access":
            "写入权限",
        "Write approvals you've granted. Each lives until either session is removed.":
            "你已授予的写入批准。任一会话移除前一直有效。",
        "Writing to other sessions":
            "写入其他会话",
        "You see what the agent does, live.":
            "可实时看到智能体的操作。",
        "agent-browser %@ at %@, installed and verified by the Host.":
            "agent-browser %@ 位于 %@，已由 Host 安装并校验。",
        "example.com, *.example.com":
            "example.com, *.example.com",
        "unknown error":
            "未知错误",

        // Appearance / Remote / Transcripts / Notifications / Features / Advanced (fill gaps)
        "Never":
            "从不",
        "After %d minutes":
            "%d 分钟后",
        "After 1 hour":
            "1 小时后",
        "After 1 day":
            "1 天后",
        "After %d hours":
            "%d 小时后",
        "None":
            "无",
        "%@'s appearance, edited from this window. Mode and color preview here while %@ is active; transparency renders in %@'s own windows.":
            "在此窗口编辑 %@ 的外观。%@ 处于活动状态时可在此预览模式与颜色；透明度在 %@ 自己的窗口中呈现。",
        "Use %@'s appearance":
            "使用 %@ 的外观",
        "Inherits from %@":
            "继承自 %@",
        "%1$@ uses %2$@'s appearance until a setting below is changed. Revert drops %1$@'s own mode, transparency and font; its color stays.":
            "%1$@ 在更改下方某项设置前使用 %2$@ 的外观。还原会丢弃 %1$@ 自己的模式、透明度与字体；颜色保留。",
        "Applies to %@'s window, sidebar and terminal colors.":
            "应用于 %@ 的窗口、侧栏与终端颜色。",
        "Washes %@'s window chrome — and its dot in the workspace pickers.":
            "为 %@ 的窗口边框上色 — 也会出现在工作区选择器中的色点上。",
        "Applies to %@'s own windows — a running instance updates live.":
            "应用于 %@ 自己的窗口 — 正在运行的实例会即时更新。",
        "%1$@'s terminals — family and size; a running instance updates live. ⌘+ / ⌘− / ⌘0 in %2$@'s own windows edit the same value.":
            "%1$@ 的终端 — 字体族与大小；正在运行的实例会即时更新。在 %2$@ 自己的窗口中用 ⌘+ / ⌘− / ⌘0 编辑同一数值。",
        "the selected workspace":
            "所选工作区",
        "Auto-stop inactive terminals":
            "自动停止空闲终端",
        "Stopped sessions to keep":
            "保留的已停止会话数",
        "Cleanup":
            "清理",
        "Idle sessions on %@ get the same treatment as Stop: the row stays, ready to Resume. Each project keeps its newest stopped sessions up to the limit; older ones are removed (pinned and unread ones are kept).":
            "%@ 上的空闲会话与「停止」相同：行仍保留，可随时「继续」。每个项目按上限保留最新已停止会话；更旧的会被移除（置顶与未读会保留）。",
        "Waiting for %@'s settings…":
            "正在等待 %@ 的设置…",
        "Writes to other sessions":
            "写入其他会话",
        "Ask":
            "询问",
        "Deny":
            "拒绝",
        "Agents may create worktrees":
            "智能体可创建工作树",
        "On":
            "开",
        "Browser screenshots go to gallery":
            "浏览器截图进入画廊",
        "A session's conversation, rendered as Markdown — what \"Copy transcript\" copies and what agents read.":
            "将会话对话渲染为 Markdown — 「复制对话记录」复制的内容，也是智能体读取的内容。",
        "Session info header":
            "会话信息标题",
        "User messages":
            "用户消息",
        "Assistant messages":
            "助手消息",
        "Reasoning":
            "推理",
        "The agent's thinking blocks.":
            "智能体的思考块。",
        "Tool calls & results":
            "工具调用与结果",
        "Commands the agent ran and their output.":
            "智能体执行的命令及其输出。",
        "File changes & diffs":
            "文件更改与差异",
        "Plan updates":
            "计划更新",
        "Range":
            "范围",
        "How much of the conversation to include.":
            "包含多少对话内容。",
        "Transcript content":
            "对话记录内容",
        "What \"Copy transcript\" (right-click a session) puts on the clipboard as Markdown. These options also drive the defaults for agents reading a session's transcript. Range is the default for agent reads; the Copy transcript menu picks its own range.":
            "「复制对话记录」（右键会话）以 Markdown 放入剪贴板的内容。这些选项也驱动智能体读取会话对话记录的默认值。「范围」是智能体读取的默认值；复制菜单可自选范围。",
        "Use %@'s notifications":
            "使用 %@ 的通知",
        "Flag menus waiting for a choice":
            "标记等待选择的菜单",
        "Show the yellow attention dot when an agent draws a pick-an-option menu. These prompts send no signal on their own, so Unpeel reads them off the screen.":
            "当智能体绘制“选择选项”菜单时显示黄色注意圆点。这些提示本身不发信号，因此 Unpeel 从屏幕读取。",
        "Attention":
            "注意",
        "When a session is waiting for you to answer an on-screen menu.":
            "当会话等待你回答屏幕上的菜单时。",
        "Use %@'s features":
            "使用 %@ 的功能",
        "Experimental":
            "实验性",
        "the selected Host":
            "所选 Host",
        "How Controllers look while scoped to %@. The Host stores these values, so another Mac or phone uses the same workspace appearance.":
            "作用域为 %@ 时 Controller 的外观。Host 存储这些值，因此另一台 Mac 或手机使用相同的工作区外观。",
        "Applies to this Controller's window, sidebar and terminal colors while %@ is active.":
            "在 %@ 处于活动状态时，应用于此 Controller 的窗口、侧栏与终端颜色。",
        "Washes the Controller chrome and identifies %@ in workspace pickers.":
            "为 Controller 边框上色，并在工作区选择器中标识 %@。",
        "Choose which Host-side App opens each supported type in this workspace.":
            "选择在此工作区中由哪个 Host 端 App 打开各支持的类型。",
        "Controls this Controller's background and terminal surface whenever %@ is selected.":
            "每当选中 %@ 时，控制此 Controller 的背景与终端表面。",
        "Fonts render on this Mac, so this is this Controller's own setting: it applies to %@'s terminals and every other workspace alike. ⌘+ and ⌘− zoom all panes; ⌘0 returns to %d pt.":
            "字体在本机渲染，因此这是此 Controller 自己的设置：同样应用于 %@ 的终端及其他所有工作区。⌘+ / ⌘− 缩放所有窗格；⌘0 恢复为 %d pt。",
        "Waiting for %@'s appearance…":
            "正在等待 %@ 的外观…",
        "How %@ flags sessions that need you, plus delivery diagnostics for this Controller Mac.":
            "%@ 如何标记需要你关注的会话，以及此 Controller Mac 的送达诊断。",
        "Show the yellow attention dot when an agent draws a pick-an-option menu on the Host.":
            "当智能体在 Host 上绘制“选择选项”菜单时显示黄色注意圆点。",
        "Applied by the Host to its session activity before every Controller receives it.":
            "由 Host 在会话活动送达每个 Controller 之前应用。",
        "Sending…":
            "发送中…",
        "Send a test notification on this Mac":
            "在本机发送测试通知",
        "Last Mac test":
            "上次 Mac 测试",
        "Open Mac Notification Settings…":
            "打开 Mac 通知设置…",
        "Tests the notification banner on the Controller you are using now; it does not run anything on %@.":
            "在你当前使用的 Controller 上测试通知横幅；不会在 %@ 上运行任何内容。",
        "Phone registration":
            "手机注册",
        "Host delivery":
            "Host 送达",
        "Phone delivery is shown only when the Host advertises it. Upstash/Linux Hosts can still surface attention here without pretending to own an APNs path.":
            "仅当 Host 声明支持时才显示手机送达。Upstash/Linux Host 仍可在此显示注意状态，而不会假装拥有 APNs 路径。",
        "Optional features owned by %@. Session-tool changes apply to sessions started after the toggle.":
            "由 %@ 拥有的可选功能。会话工具更改仅应用于切换后启动的会话。",
        "No optional features are available in this build.":
            "此构建中没有可用的可选功能。",
        "Waiting for %@'s feature settings…":
            "正在等待 %@ 的功能设置…",
        "Reconnecting to %@…":
            "正在重新连接 %@…",
        " (Not installed)":
            "（未安装）",
        "Default Editor":
            "默认编辑器",
        "System Default":
            "系统默认",
        "Update to %@":
            "更新到 %@",
        "Installed version unknown":
            "已安装版本未知",
        "Markdown":
            "Markdown",
        "CSV":
            "CSV",
        "Folders":
            "文件夹",
        "Git changes":
            "Git 更改",
        "GitHub repositories":
            "GitHub 仓库",
        "This workspace uses the default workspace's appearance until a setting below is changed. Revert drops its own mode, transparency and font; its color stays.":
            "此工作区在更改下方某项设置前使用默认工作区的外观。还原会丢弃其自己的模式、透明度与字体；颜色保留。",
        "Session gallery":
            "会话画廊",
        "Photo chip in the terminal title bar with the session's captures, plus Take Screenshot (⇧⌘S) to shoot into the session and attach it to the prompt. Turn off if you use your own screenshot tools.":
            "终端标题栏中的照片芯片显示会话截图，另有「截取屏幕」(⇧⌘S) 拍进会话并附到提示。若使用自有截图工具可关闭。",
        "Choose what ⌘T opens and configure extras around the terminal view.":
            "选择 ⌘T 打开的内容，并配置终端视图周围的附加项。",
        "Font":
            "字体",
        "Size":
            "大小",
        "Line height":
            "行高",
        "Extra space between rows, as a percentage of the font's own cell height.":
            "行之间的额外间距，按字体单元高度的百分比计。",
        "Terminal font":
            "终端字体",
        "Default (%@)":
            "默认（%@）",
        "%@ (not installed)":
            "%@（未安装）",
        "Family and size for every terminal on this Mac. ⌘+ and ⌘− zoom all panes together; ⌘0 returns to %d pt. Ghostty falls back to other installed faces for glyphs the chosen font lacks.":
            "本机每个终端的字体族与大小。⌘+ / ⌘− 一并缩放所有窗格；⌘0 恢复为 %d pt。若所选字体缺少字形，Ghostty 会回退到其他已安装字体。",
        "Auto":
            "自动",
        "Let another Unpeel device control %1$@. Devices you pair here reach %2$@ only — each workspace pairs its own devices.":
            "允许另一台 Unpeel 设备控制 %1$@。在此配对的设备仅能访问 %2$@ — 每个工作区各自配对设备。",
        "Let another Unpeel device control %1$@. %2$@ mints the credentials; this Mac only forwards the one-time sealed exchange.":
            "允许另一台 Unpeel 设备控制 %1$@。由 %2$@ 签发凭据；本机仅转发一次性密封交换。",
        "Let another Unpeel device control this workspace.":
            "允许另一台 Unpeel 设备控制此工作区。",
        "Let another Unpeel device control this Mac.":
            "允许另一台 Unpeel 设备控制此 Mac。",
        "No devices are paired with %@.":
            "没有设备与 %@ 配对。",
        "Pair a Device with %@…":
            "将设备与 %@ 配对…",
        "Controls %@":
            "控制 %@",
        "Each workspace pairs its own devices — a device paired here reaches only %1$@. Revoke devices from %1$@'s own Remote Control settings.":
            "每个工作区各自配对设备 — 在此配对的设备仅能访问 %1$@。从 %1$@ 自己的「远程控制」设置中撤销设备。",
        "Unpeel Link enrollment for %1$@'s devices lives in %1$@'s own Remote Control settings; the license covers every workspace on this Mac and is managed from %2$@'s scope.":
            "%1$@ 设备的 Unpeel Link 注册位于 %1$@ 自己的「远程控制」设置中；许可证覆盖本机所有工作区，并从 %2$@ 的作用域管理。",
        "Each workspace pairs its own devices — a device paired here reaches only %1$@, and its entry is revocable on %1$@ itself.":
            "每个工作区各自配对设备 — 在此配对的设备仅能访问 %1$@，并可在 %1$@ 自身撤销其条目。",
        "Unpeel for iPhone is in beta":
            "Unpeel for iPhone 处于测试版",
        "Join the TestFlight beta to control this workspace from your phone. Open the invite link on your iPhone to install it with TestFlight.":
            "加入 TestFlight 测试版以从手机控制此工作区。在 iPhone 上打开邀请链接，用 TestFlight 安装。",
        "Join the TestFlight beta to control this Mac from your phone. Open the invite link on your iPhone to install it with TestFlight.":
            "加入 TestFlight 测试版以从手机控制此 Mac。在 iPhone 上打开邀请链接，用 TestFlight 安装。",
        "Join the Beta":
            "加入测试版",
        "Local access":
            "本地访问",
        "Unavailable":
            "不可用",
        "Serving on this network":
            "正在此网络上提供服务",
        "No paired devices.":
            "没有已配对设备。",
        "Link":
            "Link",
        "Direct only":
            "仅直连",
        "Share This Workspace…":
            "共享此工作区…",
        "Share This Mac…":
            "共享此 Mac…",
        "Each workspace on this Mac is shared separately.":
            "本机上的每个工作区单独共享。",
        "Controls This Workspace":
            "控制此工作区",
        "Controls This Mac":
            "控制此 Mac",
        "Devices pair directly over your network and receive their own revocable credential. Revoking one immediately invalidates it.":
            "设备通过你的网络直接配对，并获得各自可撤销的凭据。撤销会立即使其失效。",
        "Authentication":
            "身份验证",
        "Stored token":
            "已存储令牌",
        "Security":
            "安全",
        "The hook and MCP servers stay localhost-only. Remote Controllers use a separate LAN server.":
            "钩子与 MCP 服务器仅限本机。远程 Controller 使用单独的局域网服务器。",
        "Share This Workspace":
            "共享此工作区",
        "Share This Mac":
            "共享此 Mac",
        "Controller paired":
            "Controller 已配对",
        "The displayed one-time code has been consumed.":
            "显示的一次性代码已使用。",
        "Pair Another Controller":
            "配对另一台 Controller",
        "Scan or paste this code into another Unpeel device.":
            "扫描或将此代码粘贴到另一台 Unpeel 设备。",
        "The code expires in five minutes and can be used once. After pairing, that Controller receives its own revocable device token.":
            "代码五分钟后过期且只能使用一次。配对后，该 Controller 获得自己的可撤销设备令牌。",
        "Or connect from a terminal":
            "或从终端连接",
        "Copied":
            "已复制",
        "Run this on another machine with the Unpeel CLI installed (curl -fsSL https://unpeel.com/install.sh | sh). It rides your normal SSH access instead of a pairing code, so this Mac needs Remote Login on (System Settings ▸ General ▸ Sharing) and your SSH config must reach it — over a VPN or Tailscale too, but never through Unpeel Link.":
            "在已安装 Unpeel CLI 的另一台机器上运行（curl -fsSL https://unpeel.com/install.sh | sh）。它走普通 SSH 访问而非配对码，因此本机需开启远程登录（系统设置 ▸ 通用 ▸ 共享），且 SSH 配置需能到达它 — 也可经 VPN 或 Tailscale，但不要经 Unpeel Link。",
        "Pair a Device with %@":
            "将设备与 %@ 配对",
        "The device pairs with %@ only — each workspace pairs its own devices.":
            "设备仅与 %@ 配对 — 每个工作区各自配对设备。",
        "Try Again":
            "重试",
        "Pair Another Device":
            "配对另一台设备",
        "Waiting for %@… Its instance starts in the background if it isn't running.":
            "正在等待 %@… 若未运行，其实例会在后台启动。",
        "The code expires in five minutes and can be used once. The device then holds its own revocable entry in %@'s device list.":
            "代码五分钟后过期且只能使用一次。设备随后在 %@ 的设备列表中保有自己的可撤销条目。",
        "Nothing is on Link — every connection stays direct, on your own network.":
            "未使用 Link — 所有连接都保持直连，在你自己的网络上。",
        "Relay":
            "中继",
        "Add to Link…":
            "添加到 Link…",
        "Reachable outside your network (Unpeel Link)":
            "可在网络外访问（Unpeel Link）",
        "No Code":
            "无代码",
        "How Unpeel flags a session that needs you and when it sends a notification banner.":
            "Unpeel 如何标记需要你关注的会话，以及何时发送通知横幅。",
        "This workspace uses the default workspace's notification settings until a setting below is changed. Revert drops its own values.":
            "此工作区在更改下方某项设置前使用默认工作区的通知设置。还原会丢弃其自己的值。",
        "Send a test Mac notification":
            "发送测试 Mac 通知",
        "Send a test phone notification":
            "发送测试手机通知",
        "Paired phone tokens":
            "已配对手机令牌",
        "Last phone push":
            "上次手机推送",
        "Last attempt":
            "上次尝试",
        "Turn Unpeel's optional features on or off — no restart needed.":
            "打开或关闭 Unpeel 的可选功能 — 无需重启。",
        "This workspace uses the default workspace's features until a toggle below is changed. Revert drops its own values.":
            "此工作区在更改下方某项开关前使用默认工作区的功能。还原会丢弃其自己的值。",
        "No optional features right now. Check back after an update.":
            "目前没有可选功能。更新后再来看看。",
        "Resource usage, cleanup, and on-disk data for Unpeel's terminal hosts.":
            "Unpeel 终端主机的资源占用、清理与磁盘数据。",
        "Sessions that have stayed idle for the selected time are stopped — the same as clicking \"Stop\": the terminal stops and the row stays in the sidebar, where Resume continues the conversation. Sessions that keep working (including loops — any activity resets the clock), or that are pinned, selected, unread, or waiting for input, are left alone; plain shell terminals are never touched. Each project keeps its newest stopped sessions up to the limit; older ones are removed automatically, except pinned and unread ones. The agent's own history (for example claude --resume) still has those conversations.":
            "空闲达到所选时长的会话会被停止 — 与点击「停止」相同：终端停止，侧栏中的行保留，可「继续」对话。仍在工作的会话（含循环 — 任何活动都会重置计时），或已置顶、选中、未读、等待输入的会话不受影响；普通 shell 终端永不触碰。每个项目按上限保留最新已停止会话；更旧的会自动移除（置顶与未读除外）。智能体自身历史（例如 claude --resume）仍保留那些对话。",
        "Memory":
            "内存",
        "Current process memory usage and active session counts.":
            "当前进程内存占用与活动会话数。",
        "Live terminal hosts sorted by current CPU usage.":
            "按当前 CPU 使用率排序的活动终端主机。",
        "Running Terminals":
            "运行中的终端",
        "PID %@":
            "PID %@",
        "%d proc":
            "%d 进程",
        "Sessions folder":
            "会话文件夹",
        "Hooks trace log":
            "钩子跟踪日志",
        "Diagnostics":
            "诊断",
        "Quick access to Unpeel's on-disk session data and hook trace log.":
            "快速访问 Unpeel 磁盘上的会话数据与钩子跟踪日志。",
        "No folder":
            "无文件夹",
        "Blank shell":
            "空白 shell",
        "Remove from Link":
            "从 Link 移除",
        "Show in Finder":
            "在 Finder 中显示",

        // Features tab + Appearance tint / Remote help
        "Git worktrees":
            "Git 工作树",
        "Sessions use":
            "Sessions 使用",
        "Browser use":
            "Browser 使用",
        "Remote workspaces":
            "远程工作区",
        "Run sessions in an isolated git worktree of a project so multiple agents can work the same repo in parallel without touching each other's files. Adds worktree controls to the project menu, sidebar, and the Worktrees settings tab.":
            "在项目的隔离 git 工作树中运行会话，让多个智能体并行处理同一仓库且互不干扰彼此文件。在项目菜单、侧栏与「工作树」设置页加入工作树控件。",
        "Let an agent session see your other sessions: it can read them all, and asks before writing to another session unless you already approved that pair. These are cooperation controls, not a sandbox against commands running as your macOS user. Adds the Sessions settings tab. Applies when a session starts, so already-running sessions pick it up after a restart.":
            "让智能体会话看到你的其他会话：可全部读取，写入另一会话前会询问（除非你已批准该对）。这些是协作控制，不是针对以你的 macOS 用户身份运行命令的沙箱。添加 Sessions 设置页。在会话启动时生效，已在运行的会话需重启后生效。",
        "Use extra, fully separate workspaces on this Mac — each workspace has its own sessions, projects, presets, settings, and pairs with your phone as its own workspace. Adds the Workspaces settings tab.":
            "在本机使用额外的完全独立工作区 — 每个工作区有各自的会话、项目、预设、设置，并作为独立工作区与手机配对。添加「工作区」设置页。",
        "Unpeel computer use has been retired.":
            "Unpeel 的 Computer use 已停用。",
        "Let agent sessions drive a real browser — open pages, click, fill forms, and take screenshots. Each session gets its own isolated browser with no access to your normal browser profile. Browser access prompts are cooperation controls, not a sandbox against commands running as your macOS user. Adds the Browser settings tab.":
            "让智能体会话操控真实浏览器 — 打开页面、点击、填写表单、截图。每个会话有独立浏览器，无法访问你的日常浏览器配置。Browser 访问提示是协作控制，不是针对以你的 macOS 用户身份运行命令的沙箱。添加 Browser 设置页。",
        "Add and control workspaces on other machines — pair another Mac, a headless `unpeel serve` box, or an SSH host — and share this Mac with other devices. Direct connections are for your own network or VPN; Unpeel Link carries the encrypted path when you are away.":
            "添加并控制其他机器上的工作区 — 配对另一台 Mac、无界面的 `unpeel serve` 主机或 SSH 主机 — 并与其他设备共享本机。直连用于你自己的网络或 VPN；外出时由 Unpeel Link 承载加密路径。",
        "Early features that are still being shaped. They can change or disappear between releases. Turn one off here if it gets in the way.":
            "仍在打磨中的早期功能。版本之间可能变更或消失。若碍事可在此关闭。",
        "Peel":
            "Peel",
        "Indigo":
            "靛蓝",
        "The active workspace — the workspace settings above the This Mac divider follow it":
            "当前活动工作区 — 「此 Mac」分隔线上方的工作区设置跟随它",
        "Take this device off Unpeel Link — it then connects only over your own network. Re-add it any time with “Add to Link…”.":
            "将此设备从 Unpeel Link 移除 — 之后仅通过你自己的网络连接。随时可用「添加到 Link…」重新加入。",

        // Color names
        "Green": "绿色",

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
