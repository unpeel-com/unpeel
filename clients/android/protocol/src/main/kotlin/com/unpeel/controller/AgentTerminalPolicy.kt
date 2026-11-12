package com.unpeel.controller

/**
 * Phone terminal behavior copied from iOS `RemoteGhosttyRenderer`.
 * Wheel forwarding follows the Host's modes once a mode snapshot has arrived.
 * The named agents stay a guess until that snapshot, or until the stream
 * turns mouse tracking or the alternate screen off. Menu detection matches
 * the phone scan, including the requirement that numbered choices are on screen.
 */
object AgentTerminalPolicy {
    enum class Wheel {
        /** Local scrollback. */
        None,
        /** SGR (or classic) wheel reports. */
        Mouse,
        /** Cursor up/down, application form when DECCKM is on. Alt screen only. */
        AlternateScroll,
    }

    private val remoteMouseWheelProviders = setOf("claude", "grok", "opencode")

    private val navMarkers = listOf("to navigate", "↑/↓", "↑ ↓", "▲/▼", "arrow keys")
    private val selectMarkers = listOf(
        "to select", "to confirm", "to choose", "enter to", "esc to cancel", "return to",
    )
    private val confirmMarkers = listOf("enter to confirm", "return to confirm")
    private val cancelMarkers = listOf("esc to cancel", "escape to cancel")
    private val amendMarkers = listOf("tab to amend")
    private val passiveMarkers = listOf("to view")
    private val passiveSelectorPrefixes = listOf("↑/↓ to select")
    private val interactiveQualifiers = listOf(
        "to navigate", "to confirm", "to choose", "esc to cancel", "escape to cancel",
    )

    fun prefersRemoteMouseWheel(providerID: String?, command: String, activeRuntimeID: String?): Boolean {
        val runtime = activeRuntimeID?.lowercase()
        if (runtime != null && runtime in remoteMouseWheelProviders) return true
        val provider = providerID.orEmpty().lowercase()
        if (provider.isNotEmpty() && provider in remoteMouseWheelProviders) return true
        val head = command.trim().split(Regex("\\s+")).firstOrNull()
            ?.substringAfterLast('/')
            ?.lowercase()
            .orEmpty()
        return head.isNotEmpty() && head in remoteMouseWheelProviders
    }

    fun wheelForwarding(
        modesKnown: Boolean,
        mouseTracking: Boolean,
        alternateScreen: Boolean,
        sawMouseOrAlternateDisable: Boolean,
        prefersRemoteMouseWheel: Boolean,
    ): Wheel {
        if (mouseTracking) return Wheel.Mouse
        if (alternateScreen) return Wheel.AlternateScroll
        if (modesKnown) return Wheel.None
        return if (prefersRemoteMouseWheel && !sawMouseOrAlternateDisable) Wheel.Mouse else Wheel.None
    }

    /** Phone gate: a footer is not enough. At least two numbered choices must be visible. */
    fun viewportHasMenuPrompt(text: String): Boolean {
        if (highestMenuOptionNumber(text) < 2) return false
        val lines = text.split('\n').map { it.lowercase() }
        for (index in lines.indices) {
            val raw = if (index + 1 < lines.size) lines[index] + "\n" + lines[index + 1] else lines[index]
            val window = raw.split(Regex("\\s+")).filter { it.isNotEmpty() }.joinToString(" ")
            val hasNav = navMarkers.any { window.contains(it) }
            val hasSelect = selectMarkers.any { window.contains(it) }
            val hasConfirm = confirmMarkers.any { window.contains(it) }
            val hasCancel = cancelMarkers.any { window.contains(it) }
            val hasAmend = amendMarkers.any { window.contains(it) }
            val passiveAction = passiveMarkers.any { window.contains(it) }
            val passivePrefix = passiveSelectorPrefixes.any { window.contains(it) }
            val interactive = interactiveQualifiers.any { window.contains(it) }
            val passive = passiveAction || (passivePrefix && !interactive)
            val approval = hasCancel && hasAmend && hasSelectedChoices(lines, index)
            if (((hasNav && hasSelect) || (hasConfirm && hasCancel) || approval) && !passive) return true
        }
        return false
    }

    fun highestMenuOptionNumber(text: String): Int {
        var highest = 0
        for (raw in text.lineSequence()) {
            var index = 0
            while (index < raw.length) {
                val ch = raw[index]
                if (ch != ' ' && ch != '\t' && ch != '❯' && ch != '>' && ch != '•') break
                index++
            }
            val start = index
            while (index < raw.length && raw[index].isDigit()) index++
            if (index == start || index >= raw.length || raw[index] != '.') continue
            val value = raw.substring(start, index).toIntOrNull() ?: continue
            if (value in 1..9) highest = maxOf(highest, value)
        }
        return highest
    }

    private fun hasSelectedChoices(lines: List<String>, footer: Int): Boolean {
        var previous: Int? = null
        var count = 0
        var selected = false
        val start = maxOf(0, footer - 12)
        for (raw in lines.subList(start, footer)) {
            val line = raw.trim()
            val cursor = line.firstOrNull()?.let { it == '❯' || it == '›' || it == '>' } == true
            val choice = line.dropWhile { it == '❯' || it == '›' || it == '>' }.dropWhile { it.isWhitespace() }
            val dot = choice.indexOf('.')
            if (dot <= 0) continue
            val number = choice.substring(0, dot).toIntOrNull() ?: continue
            if (number !in 1..9) continue
            val label = choice.substring(dot + 1)
            if (label.firstOrNull()?.isWhitespace() != true || label.isBlank()) continue
            if (previous != number - 1) {
                count = 0
                selected = false
            }
            previous = number
            count += 1
            selected = selected || cursor
        }
        return count >= 2 && selected
    }
}
