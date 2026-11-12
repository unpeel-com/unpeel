package com.unpeel.controller

/**
 * Arrow keys that walk the cursor from the cell the host has drawn to the
 * cell under the finger. The remote program is what actually moves it.
 * A normal shell only walks the current row: up and down there are history.
 */
object CursorMotion {
    const val MAX_STEPS = 48

    data class Step(val keys: String, val col: Int, val row: Int)

    fun step(
        fromCol: Int,
        fromRow: Int,
        toCol: Int,
        toRow: Int,
        allowRows: Boolean,
        applicationCursor: Boolean,
    ): Step {
        val rowDelta = if (allowRows) toRow - fromRow else 0
        val colDelta = toCol - fromCol
        val rows = kotlin.math.abs(rowDelta)
        val cols = kotlin.math.abs(colDelta)
        val budget = minOf(rows + cols, MAX_STEPS)
        val rowSteps = minOf(rows, budget)
        val colSteps = minOf(cols, budget - rowSteps)
        val keys = arrow(if (rowDelta < 0) 'A' else 'B', applicationCursor).repeat(rowSteps) +
            arrow(if (colDelta < 0) 'D' else 'C', applicationCursor).repeat(colSteps)
        val rowSign = if (rowDelta < 0) -1 else 1
        val colSign = if (colDelta < 0) -1 else 1
        return Step(
            keys = keys,
            col = fromCol + colSign * colSteps,
            row = fromRow + rowSign * rowSteps,
        )
    }

    private fun arrow(direction: Char, applicationCursor: Boolean): String {
        return if (applicationCursor) "\u001bO$direction" else "\u001b[$direction"
    }
}
