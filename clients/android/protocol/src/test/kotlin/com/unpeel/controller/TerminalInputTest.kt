package com.unpeel.controller

import org.junit.Assert.assertEquals
import org.junit.Test

class TerminalInputTest {
    @Test
    fun plainEnterIsCarriageReturn() {
        assertEquals("\r", TerminalInput.key("enter"))
    }

    @Test
    fun ctrlEnterUsesModifyOtherKeys() {
        assertEquals("\u001b[27;5;13~", TerminalInput.ctrlKey("enter"))
    }

    @Test
    fun ctrlTabMatchesSoftKeyRow() {
        assertEquals("\u001b[27;5;9~", TerminalInput.ctrlKey("tab"))
    }
}
