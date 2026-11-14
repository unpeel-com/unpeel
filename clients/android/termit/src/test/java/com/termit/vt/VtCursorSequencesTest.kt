package com.termit.vt

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Test

/** Sequences a differential TUI redraw relies on to land text on the right cell. */
class VtCursorSequencesTest {
    private fun vt(cols: Int = 20, rows: Int = 6) = VtEmulator(cols, rows)

    private fun VtEmulator.feedText(text: String) {
        val bytes = text.toByteArray(Charsets.UTF_8)
        append(bytes, bytes.size)
    }

    private fun VtEmulator.rowText(row: Int): String =
        (0 until mColumns).joinToString("") { col ->
            val ch = screen.cell(row, col).char
            if (ch == '\u0000') "" else ch.toString()
        }.trimEnd()

    @Test
    fun cursorNextLineMovesDownAndToColumnOne() {
        val t = vt()
        t.feedText("\u001b[1;5Hab\u001b[2Ecd")
        assertEquals("    ab", t.rowText(0))
        assertEquals("cd", t.rowText(2))
    }

    @Test
    fun cursorPrecedingLineMovesUpAndToColumnOne() {
        val t = vt()
        t.feedText("\u001b[4;9Hx\u001b[2Fy")
        assertEquals("y", t.rowText(1))
        assertEquals("        x", t.rowText(3))
    }

    @Test
    fun horizontalPositionAbsoluteAndRelative() {
        val t = vt()
        t.feedText("\u001b[1;1H\u001b[6`a\u001b[2ab")
        assertEquals("     a  b", t.rowText(0))
    }

    @Test
    fun verticalPositionRelativeMovesDown() {
        val t = vt()
        t.feedText("a\u001b[3eb")
        assertEquals("a", t.rowText(0))
        assertEquals(" b", t.rowText(3))
    }

    @Test
    fun repeatPrintsTheLastCharacter() {
        val t = vt()
        t.feedText("x\u001b[4b")
        assertEquals("xxxxx", t.rowText(0))
    }

    @Test
    fun repeatWithNothingPrintedDoesNothing() {
        val t = vt()
        t.feedText("\u001b[5b")
        assertEquals("", t.rowText(0))
    }

    @Test
    fun backTabGoesToThePreviousStop() {
        val t = vt(cols = 30)
        t.feedText("\u001b[1;20H\u001b[Za")
        assertEquals(16, t.rowText(0).indexOf('a'))
    }

    @Test
    fun customTabStopIsHonoured() {
        val t = vt(cols = 30)
        t.feedText("\u001b[1;4H\u001bH\u001b[1;1H\tz")
        assertEquals(3, t.rowText(0).indexOf('z'))
    }

    @Test
    fun clearAllTabStopsSendsTabToTheLastColumn() {
        val t = vt(cols = 12)
        t.feedText("\u001b[3g\u001b[1;1H\tz")
        assertEquals(11, t.rowText(0).indexOf('z'))
    }

    @Test
    fun apcBodyIsNotPrinted() {
        val t = vt()
        t.feedText("a\u001b_Gi=1;payload\u001b\\b")
        assertEquals("ab", t.rowText(0))
    }

    @Test
    fun sosAndPmBodiesAreNotPrinted() {
        val t = vt()
        t.feedText("a\u001bXhidden\u001b\\b\u001b^also\u001b\\c")
        assertEquals("abc", t.rowText(0))
    }

    @Test
    fun oscTitleWithBrailleEllipsisAndCjkIsNotPrintedAndArrivesDecoded() {
        val t = vt()
        var title: String? = null
        t.onTitle = { title = it }
        t.feedText("ab\u001b]0;⠹ - 处理中… - grok\u0007cd")
        assertEquals("abcd", t.rowText(0))
        assertEquals("⠹ - 处理中… - grok", title)
    }

    @Test
    fun stTerminatedOscWithNonAsciiIsNotPrinted() {
        val t = vt()
        t.feedText("a\u001b]2;⠋ spin…\u001b\\b")
        assertEquals("ab", t.rowText(0))
    }

    @Test
    fun dcsBodyWithNonAsciiIsNotPrinted() {
        val t = vt()
        t.feedText("a\u001bP1\$r⠹…\u001b\\b")
        assertEquals("ab", t.rowText(0))
    }

    @Test
    fun textAfterATitleStillDecodesAsUtf8() {
        val t = vt()
        t.feedText("\u001b]0;⠹\u0007处x")
        assertEquals("处x", t.rowText(0))
    }

    @Test
    fun csiWithAnIntermediateByteIsNotTheBareCommand() {
        val t = vt()
        // CSI 2 SP A is not CUU: the cursor must stay on row 3 for the x.
        t.feedText("\u001b[3;1H\u001b[2 Ax")
        assertEquals("", t.rowText(0))
        assertEquals("x", t.rowText(2))
    }

    @Test
    fun decRequestWithDollarIsNotASetScrollRegion() {
        val t = vt()
        // A real CSI r would send the cursor home; with the $ it is a different command.
        t.feedText("\u001b[3;1H\u001b[2;3\$rx")
        assertEquals("", t.rowText(0))
        assertEquals("x", t.rowText(2))
    }

    @Test
    fun oscCutOffByAnEscapeSequenceDoesNotEatItsBytes() {
        val t = vt()
        var title: String? = null
        t.onTitle = { title = it }
        t.feedText("\u001b]0;half a title\u001b[3;1Hx")
        assertEquals("", t.rowText(0))
        assertEquals("x", t.rowText(2))
        assertEquals(null, title)
    }

    @Test
    fun dcsCutOffByAnEscapeSequenceDoesNotEatItsBytes() {
        val t = vt()
        t.feedText("\u001bPq half\u001b[3;1Hx")
        assertEquals("", t.rowText(0))
        assertEquals("x", t.rowText(2))
    }

    @Test
    fun canAbortsAnOscString() {
        val t = vt()
        var title: String? = null
        t.onTitle = { title = it }
        t.feedText("\u001b]0;abc\u0018d")
        assertEquals("d", t.rowText(0))
        assertEquals(null, title)
    }

    @Test
    fun escHashAndPercentConsumeTheirFinalByte() {
        val t = vt()
        t.feedText("a\u001b#8b\u001b%Gc")
        assertEquals("abc", t.rowText(0))
    }

    @Test
    fun underlineStyleSubparameterIsNotReadAsNewCodes() {
        val t = vt()
        // 4:3 is a curly underline. The 3 must not be taken as "italic".
        t.feedText("\u001b[4:3mu\u001b[4:0mn")
        assertEquals(true, t.screen.cell(0, 0).underline)
        assertFalse(t.screen.cell(0, 1).underline)
    }

    @Test
    fun colonTruecolorWithEmptyColorspaceIsRead() {
        val t = vt()
        t.feedText("\u001b[38:2::10:20:30mx")
        assertEquals(0xff0a141e.toInt(), t.screen.cell(0, 0).fg)
    }

    @Test
    fun semicolonTruecolorStillWorks() {
        val t = vt()
        t.feedText("\u001b[48;2;1;2;3mx")
        assertEquals(0xff010203.toInt(), t.screen.cell(0, 0).bg)
    }

    @Test
    fun underlineColourParametersAreConsumedNotDrawn() {
        val t = vt()
        // 58;2;r;g;b must not leak r/g/b as SGR codes (those would recolour the text).
        t.feedText("\u001b[58;2;40;41;42;1mx")
        assertEquals(true, t.screen.cell(0, 0).bold)
        assertEquals(CellStyle.DEFAULT_FG, t.screen.cell(0, 0).fg)
        assertEquals(CellStyle.DEFAULT_BG, t.screen.cell(0, 0).bg)
    }
}
