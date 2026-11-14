package com.unpeel.android

import androidx.compose.runtime.Composable
import androidx.compose.runtime.ReadOnlyComposable
import androidx.compose.ui.platform.LocalConfiguration
import androidx.compose.ui.platform.LocalContext

internal object L10n {
    fun text(english: String, simplified: Boolean): String =
        if (simplified) zh[english] ?: english else english

    private val zh = mapOf(
        "THEME" to "主题",
        "LANGUAGE" to "语言",
        "Follow system" to "跟随系统",
        "Dark" to "深色",
        "Light" to "浅色",
        "English" to "英文",
        "SECURITY" to "安全",
        "Require biometrics" to "需要生物识别",
        "Require screen lock" to "需要屏幕锁",
        "Locks Unpeel when you leave the app." to "离开应用时锁定 Unpeel。",
        "Set a device screen lock (and enroll biometrics) in system Settings to use the app lock." to
            "请先在系统设置里设置屏幕锁并录入生物识别，才能使用应用锁。",
        "This phone has no fingerprint, face, or screen lock" to "这台手机没有指纹、面容或屏幕锁",
        "This phone has no screen lock" to "这台手机没有屏幕锁",
        "Unlock Unpeel" to "解锁 Unpeel",
        "Confirm to lock Unpeel when you leave the app" to "确认后，离开应用时锁定 Unpeel",
        "Remove" to "移除",
        "Cancel" to "取消",
        "Remove this session" to "移除这个会话",
        "Running processes will stop first. The session leaves the list; the conversation itself stays." to
            "正在运行的进程会先停止。会话会从列表里消失，对话本身还在。",
        "Can't reach %s" to "连不上 %s",
        "Not connected to a Mac" to "没有连上 Mac",
        "Pair with your Mac" to "与 Mac 配对",
        "Connection…" to "正在连接…",
        "Make sure this phone and your Mac are on the same network, or that Unpeel Link is available." to
            "请确认手机和 Mac 在同一网络，或者 Unpeel Link 可用。",
        "Connecting to %s…" to "正在连接 %s…",
        "Connecting…" to "正在连接…",
        "Open Sessions" to "打开会话",
        "Close" to "关闭",
        "Paste" to "粘贴",
        "Nothing to paste" to "没有可粘贴的内容",
        "Copy" to "复制",
        "Nothing to copy" to "没有可复制的内容",
        "Fit to this phone" to "适配这台手机",
        "Preparing" to "正在准备",
        "Workspace" to "工作区",
        "Unpaired" to "未配对",
        "Disconnected" to "已断开",
        "via Unpeel Remote" to "经由 Unpeel Remote",
        "Don’t allow" to "不允许",
        "Allow" to "允许",
        "%d more waiting" to "还有 %d 个等待",
        "Done" to "完成",
        "Pair with a workspace" to "与工作区配对",
        "Workspaces" to "工作区",
        "Pair the new workspace" to "配对新的工作区",
        "Forget %s?" to "忘记 %s？",
        "This phone will no longer be able to control this workspace. You can also revoke this phone in the workspace's Settings on the Mac." to
            "这台手机将不能再控制这个工作区。你也可以在 Mac 上这个工作区的设置里撤销这台手机。",
        "Forget %s" to "忘记 %s",
        "Not paired" to "未配对",
        "Open Unpeel on your Mac, switch to the workspace you want, and show the pairing code in Settings ▸ iPhone. Each workspace pairs separately." to
            "在 Mac 上打开 Unpeel，切换到要控制的工作区，然后在「设置 › iPhone」里显示配对码。每个工作区单独配对。",
        "Add a Workspace" to "添加工作区",
        "No active sessions" to "没有进行中的会话",
        "Blocked" to "受阻",
        "Active" to "进行中",
        "Recent" to "最近",
        "Terminal" to "终端",
        "run" to "运行",
        "now" to "刚刚",
        "Edit session" to "编辑会话",
        "Notify when done" to "完成时通知",
        "Alert you when this turn finishes" to "这一轮结束时提醒你",
        "Save" to "保存",
        "Unpin" to "取消置顶",
        "Pin" to "置顶",
        "Copy transcript" to "复制记录",
        "Last 20" to "最近 20 条",
        "Last 50" to "最近 50 条",
        "All" to "全部",
        "Copied as Markdown" to "已复制为 Markdown",
        "Resume agent" to "继续代理",
        "Resume" to "继续",
        "Stop" to "停止",
        "Remove session" to "移除会话",
        "Remove from list" to "从列表移除",
        "Stop this session" to "停止这个会话",
        "The process stops. The session stays in the list so you can resume later." to
            "进程会停止。会话留在列表里，之后可以继续。",
        "Edit group" to "编辑分组",
        "Edit folder" to "编辑文件夹",
        "Update Unpeel on the Mac first to organize folders from the phone." to
            "请先把 Mac 上的 Unpeel 更新到新版本，才能在手机上整理文件夹。",
        "Folder name" to "文件夹名称",
        "Sort sessions" to "会话排序",
        "Custom order" to "自定义顺序",
        "Date (newest first)" to "按日期（新的在前）",
        "Folder color" to "文件夹颜色",
        "New session" to "新会话",
        "This Host has no presets available." to "这个 Host 没有可用的预设。",
        "Scan the pairing code" to "扫描配对码",
        "Camera permission is required to scan a pairing code" to "扫描配对码需要相机权限",
        "Or paste the pairing code" to "或粘贴配对码",
        "Pairing code" to "配对码",
        "Connect" to "连接",
        "Locked" to "已锁定",
        "Authenticate to keep viewing sessions" to "验证后才能继续查看会话",
        "Verifying…" to "正在验证…",
        "Unlock" to "解锁",
        "Resuming…" to "正在继续…",
        "Session ended" to "会话已结束",
        "Feedback & bugs" to "反馈与问题",
        "Connection lost" to "连接已断开",
        "Pair a Mac first — sessions will show up here." to "请先配对一台 Mac，会话会出现在这里。",
        "Connecting to this Mac." to "正在连接这台 Mac。",
        "Reconnecting automatically. Your sessions will reappear once your Mac is reachable." to
            "正在自动重连。Mac 恢复可达后，会话会重新出现。",
        "Couldn't read that photo" to "读不了这张照片",
        "Couldn’t read that pairing code" to "读不了这个配对码",
        "That pairing code has expired" to "这个配对码已经过期",
        "Image is over 4 MB" to "图片超过 4 MB",
        "Couldn't prepare this image for upload." to "没法准备这张图片上传。",
        "Couldn't read the selected image." to "读不了选中的图片。",
        "Resume agent keeps this terminal. Stop leaves the session in the list so you can resume. Remove deletes it." to
            "继续代理会留在这个终端。停止后会话留在列表里，可以再继续。移除会删掉它。",
        "Resume agent keeps this terminal. Remove stops and deletes this session." to
            "继续代理会留在这个终端。移除会停止并删掉这个会话。",
        "Stop leaves the session in the list so you can resume. Remove stops and deletes it." to
            "停止后会话留在列表里，可以再继续。移除会停止并删掉它。",
        "This launch has nothing to resume. Remove stops and deletes it." to
            "这次启动没有可以继续的内容。移除会停止并删掉它。",
        "Resume continues this conversation. Remove only clears it from the list." to
            "继续会接着这段对话。移除只是从列表里清掉。",
        "Remove only clears it from the list." to "移除只是从列表里清掉。",
        "Session gallery" to "会话图库",
        "Screenshots the agent's browser and device capture, plus images you add or mark up." to
            "代理浏览器和设备截到的图，以及你添加或标注的图片。",
        "Add image" to "添加图片",
        "Add to message" to "加到消息",
        "Open" to "打开",
        "Delete" to "删除",
        "Couldn't load this file" to "无法加载这个文件",
        "Crop" to "裁剪",
        "Arrows" to "箭头",
        "Drag on the image to draw · pinch to zoom" to "在图片上拖动绘制 · 双指缩放",
        "Undo" to "撤销",
        "Clear" to "清除",
        "Reset" to "重置",
    )
}

@Composable
@ReadOnlyComposable
internal fun t(english: String): String {
    val settings = LocalPhoneSettings.current
    val simplified = if (settings == null) {
        false
    } else {
        val locale = LocalConfiguration.current.locales[0]
        PhoneAppearance.simplifiedChinese(settings.language, locale.language, locale.script, locale.country)
    }
    return L10n.text(english, simplified)
}

@Composable
@ReadOnlyComposable
internal fun phoneNight(): Boolean {
    val system = (LocalConfiguration.current.uiMode and android.content.res.Configuration.UI_MODE_NIGHT_MASK) ==
        android.content.res.Configuration.UI_MODE_NIGHT_YES
    val choice = LocalPhoneSettings.current?.theme ?: ThemeChoice.System
    return PhoneAppearance.night(choice, system)
}

@Composable
@ReadOnlyComposable
internal fun phoneSimplifiedChinese(): Boolean {
    val settings = LocalPhoneSettings.current ?: return PhoneSettings.usesSimplifiedChinese(LocalContext.current)
    val locale = LocalConfiguration.current.locales[0]
    return PhoneAppearance.simplifiedChinese(settings.language, locale.language, locale.script, locale.country)
}
