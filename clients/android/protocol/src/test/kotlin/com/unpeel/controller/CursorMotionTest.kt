package com.unpeel.controller

import org.junit.Assert.assertEquals
import org.junit.Test

class CursorMotionTest {
    @Test
    fun shellWalksOnlyTheCurrentRow() {
        val step = CursorMotion.step(
            fromCol = 4,
            fromRow = 10,
            toCol = 7,
            toRow = 2,
            allowRows = false,
            applicationCursor = false,
        )
        assertEquals("\u001b[C\u001b[C\u001b[C", step.keys)
        assertEquals(7, step.col)
        assertEquals(10, step.row)
    }

    @Test
    fun fullScreenWalksRowsThenColumns() {
        val step = CursorMotion.step(
            fromCol = 5,
            fromRow = 5,
            toCol = 3,
            toRow = 8,
            allowRows = true,
            applicationCursor = false,
        )
        assertEquals("\u001b[B\u001b[B\u001b[B\u001b[D\u001b[D", step.keys)
        assertEquals(3, step.col)
        assertEquals(8, step.row)
    }

    @Test
    fun applicationCursorUsesSs3() {
        val step = CursorMotion.step(1, 1, 1, 0, allowRows = true, applicationCursor = true)
        assertEquals("\u001bOA", step.keys)
        assertEquals(0, step.row)
    }

    @Test
    fun aLongDragStopsAtTheStepCap() {
        val step = CursorMotion.step(0, 0, 80, 0, allowRows = false, applicationCursor = false)
        assertEquals(CursorMotion.MAX_STEPS, step.keys.length / 3)
        assertEquals(CursorMotion.MAX_STEPS, step.col)
    }

    @Test
    fun sameCellSendsNothing() {
        val step = CursorMotion.step(2, 2, 2, 2, allowRows = true, applicationCursor = false)
        assertEquals("", step.keys)
        assertEquals(2, step.col)
        assertEquals(2, step.row)
    }
}
