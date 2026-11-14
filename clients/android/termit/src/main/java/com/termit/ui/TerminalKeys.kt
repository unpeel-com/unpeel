package com.termit.ui

object TerminalKeys {
    const val ENTER = "\r"
    const val TAB = "\t"
    const val ESC = "\u001b"
    const val BACKSPACE = "\u007f"
    const val UP = "\u001b[A"
    const val DOWN = "\u001b[B"
    const val RIGHT = "\u001b[C"
    const val LEFT = "\u001b[D"
    const val HOME = "\u001b[H"
    const val END = "\u001b[F"
    const val PAGE_UP = "\u001b[5~"
    const val PAGE_DOWN = "\u001b[6~"
    const val DELETE = "\u001b[3~"

    fun ctrl(letter: Char): String {
        val c = letter.lowercaseChar()
        require(c in 'a'..'z')
        return ((c.code - 'a'.code) + 1).toChar().toString()
    }
}
