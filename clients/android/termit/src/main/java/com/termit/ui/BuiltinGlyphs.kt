package com.termit.ui

import android.graphics.Canvas
import android.graphics.Paint
import android.graphics.Path
import android.graphics.RectF

/**
 * Edge-to-edge procedural glyphs so box lines meet and prompt/powerline
 * arrows stay on-cell. Phone [Typeface.MONOSPACE] leaves gaps or tofu here.
 */
internal object BuiltinGlyphs {
    // Box/block fills: no AA — faster and seams stay crisp between cells.
    private val stroke = Paint().apply {
        style = Paint.Style.STROKE
        strokeCap = Paint.Cap.BUTT
        strokeJoin = Paint.Join.MITER
        isAntiAlias = true
    }
    private val fill = Paint().apply {
        style = Paint.Style.FILL
        isAntiAlias = false
    }
    private val fillAA = Paint(Paint.ANTI_ALIAS_FLAG).apply {
        style = Paint.Style.FILL
    }
    private val path = Path()
    private val oval = RectF()

    fun handles(cp: Int): Boolean = when (cp) {
        in 0x2500..0x259F -> true // box + block
        in 0x2800..0x28FF -> true // braille
        in 0xE0B0..0xE0BF -> true // powerline
        // ←↑→↓ ▶ ❯ etc. come from JetBrains Mono (same as Mac). Only draw
        // codepoints the face lacks — otherwise our 85%-cell strokes look huge.
        0x23F4, 0x23F5, 0x23F6, 0x23F7 -> true // ⏴⏵⏶⏷ (Claude caret)
        0x27A4 -> true // ➤ missing from JBM
        // Circles JBM lacks (⚪⚫⬤⏺) fall through to Noto Color Emoji on
        // Android — orange/rounded "emoji tile". Mac keeps a text circle.
        // Also intercept ●○ etc. so emoji fallback cannot steal them.
        0x2022, 0x2219, 0x25CB, 0x25CF, 0x25C9, 0x25CE, 0x25E6,
        0x26AA, 0x26AB, 0x2B24, 0x23FA -> true
        else -> false
    }

    /**
     * @return true if drawn (caller must skip font glyph).
     * Powerline fills its own background; others assume bg already painted.
     */
    fun draw(
        canvas: Canvas,
        cp: Int,
        left: Float,
        top: Float,
        width: Float,
        height: Float,
        fg: Int,
        bg: Int,
    ): Boolean {
        if (!handles(cp)) return false
        when {
            cp in 0x2800..0x28FF -> {
                drawBraille(canvas, cp, left, top, width, height, fg)
                return true
            }
            cp in 0x2500..0x257F -> {
                drawBox(canvas, cp, left, top, width, height, fg)
                return true
            }
            cp in 0x2580..0x259F -> {
                drawBlock(canvas, cp, left, top, width, height, fg)
                return true
            }
            cp in 0xE0B0..0xE0BF -> {
                drawPowerline(canvas, cp, left, top, width, height, fg, bg)
                return true
            }
            isCircle(cp) -> {
                drawCircleGlyph(canvas, cp, left, top, width, height, fg)
                return true
            }
            else -> {
                drawArrow(canvas, cp, left, top, width, height, fg)
                return true
            }
        }
    }

    private fun isCircle(cp: Int): Boolean = when (cp) {
        0x2022, 0x2219, 0x25CB, 0x25CF, 0x25C9, 0x25CE, 0x25E6,
        0x26AA, 0x26AB, 0x2B24, 0x23FA -> true
        else -> false
    }

    /**
     * Monochrome circle in the cell's fg — same look as Mac JetBrains/text
     * circles, never the Android color-emoji badge.
     */
    private fun drawCircleGlyph(
        canvas: Canvas,
        cp: Int,
        left: Float,
        top: Float,
        width: Float,
        height: Float,
        fg: Int,
    ) {
        fillAA.color = fg
        stroke.color = fg
        stroke.strokeCap = Paint.Cap.ROUND
        val cx = left + width * 0.5f
        val cy = top + height * 0.5f
        val minSide = minOf(width, height)
        when (cp) {
            0x25CB, 0x25E6, 0x26AA -> { // ○ ◦ ⚪ hollow
                val r = minSide * 0.28f
                stroke.strokeWidth = maxOf(1.5f, minSide * 0.08f)
                canvas.drawCircle(cx, cy, r, stroke)
            }
            0x25C9 -> { // ◉ fisheye
                val r = minSide * 0.28f
                stroke.strokeWidth = maxOf(1.5f, minSide * 0.07f)
                canvas.drawCircle(cx, cy, r, stroke)
                canvas.drawCircle(cx, cy, r * 0.42f, fillAA)
            }
            0x25CE -> { // ◎ bullseye
                val r = minSide * 0.28f
                stroke.strokeWidth = maxOf(1.5f, minSide * 0.07f)
                canvas.drawCircle(cx, cy, r, stroke)
                canvas.drawCircle(cx, cy, r * 0.55f, stroke)
            }
            0x2B24, 0x23FA -> { // ⬤ ⏺ slightly larger fill
                canvas.drawCircle(cx, cy, minSide * 0.32f, fillAA)
            }
            else -> { // • ∙ ● ⚫ and friends
                val scale = when (cp) {
                    0x2022, 0x2219 -> 0.18f
                    else -> 0.28f
                }
                canvas.drawCircle(cx, cy, minSide * scale, fillAA)
            }
        }
        stroke.strokeCap = Paint.Cap.BUTT
    }

    // --- Braille (same 2×4 layout as TerminalRenderer) ---

    private fun drawBraille(
        canvas: Canvas,
        code: Int,
        left: Float,
        top: Float,
        width: Float,
        height: Float,
        fg: Int,
    ) {
        val bits = code - 0x2800
        if (bits == 0) return
        fillAA.color = fg
        val insetX = width * 0.22f
        val insetY = height * 0.14f
        val colGap = width - insetX * 2f
        val rowGap = height - insetY * 2f
        val radius = minOf(width, height) * 0.11f
        var bit = 0
        while (bit < 8) {
            if ((bits shr bit) and 1 != 0) {
                val col = if (bit < 3 || bit == 6) 0 else 1
                val row = when (bit) {
                    0, 3 -> 0
                    1, 4 -> 1
                    2, 5 -> 2
                    else -> 3
                }
                canvas.drawCircle(
                    left + insetX + col * colGap,
                    top + insetY + row * (rowGap / 3f),
                    radius,
                    fillAA,
                )
            }
            bit++
        }
    }

    // --- Box drawing ---

    // Thickness from cell width only so │ and ┌ share the same stroke.
    private fun light(w: Float, @Suppress("UNUSED_PARAMETER") h: Float) = maxOf(2f, w / 6.5f)
    private fun heavy(w: Float, @Suppress("UNUSED_PARAMETER") h: Float) = maxOf(3f, w / 3.5f)
    /** Overlap into the neighboring cell so seams cannot open under scale/AA. */
    private val EDGE = 1.75f

    private fun drawBox(
        canvas: Canvas,
        cp: Int,
        left: Float,
        top: Float,
        width: Float,
        height: Float,
        fg: Int,
    ) {
        val right = left + width
        val bottom = top + height
        val cx = left + width * 0.5f
        val cy = top + height * 0.5f
        val lt = light(width, height)
        val ht = heavy(width, height)
        fill.color = fg
        stroke.color = fg

        when (cp) {
            // Light / heavy horizontals & verticals
            0x2500 -> hLine(canvas, left, right, cy, lt) // ─
            0x2501 -> hLine(canvas, left, right, cy, ht) // ━
            0x2502 -> vLine(canvas, top, bottom, cx, lt) // │
            0x2503 -> vLine(canvas, top, bottom, cx, ht) // ┃
            // Light triple-dash
            0x2504 -> hDash(canvas, left, right, cy, lt, 3) // ┄
            0x2505 -> hDash(canvas, left, right, cy, ht, 3) // ┅
            0x2506 -> vDash(canvas, top, bottom, cx, lt, 3) // ┆
            0x2507 -> vDash(canvas, top, bottom, cx, ht, 3) // ┇
            // Light / heavy quadruple-dash
            0x2508 -> hDash(canvas, left, right, cy, lt, 4) // ┈
            0x2509 -> hDash(canvas, left, right, cy, ht, 4) // ┉
            0x250A -> vDash(canvas, top, bottom, cx, lt, 4) // ┊
            0x250B -> vDash(canvas, top, bottom, cx, ht, 4) // ┋
            // Light corners
            0x250C -> { vLine(canvas, cy, bottom, cx, lt); hLine(canvas, cx, right, cy, lt) } // ┌
            0x250D -> { vLine(canvas, cy, bottom, cx, lt); hLine(canvas, cx, right, cy, ht) }
            0x250E -> { vLine(canvas, cy, bottom, cx, ht); hLine(canvas, cx, right, cy, lt) }
            0x250F -> { vLine(canvas, cy, bottom, cx, ht); hLine(canvas, cx, right, cy, ht) } // ┏
            0x2510 -> { vLine(canvas, cy, bottom, cx, lt); hLine(canvas, left, cx, cy, lt) } // ┐
            0x2511 -> { vLine(canvas, cy, bottom, cx, lt); hLine(canvas, left, cx, cy, ht) }
            0x2512 -> { vLine(canvas, cy, bottom, cx, ht); hLine(canvas, left, cx, cy, lt) }
            0x2513 -> { vLine(canvas, cy, bottom, cx, ht); hLine(canvas, left, cx, cy, ht) } // ┓
            0x2514 -> { vLine(canvas, top, cy, cx, lt); hLine(canvas, cx, right, cy, lt) } // └
            0x2515 -> { vLine(canvas, top, cy, cx, lt); hLine(canvas, cx, right, cy, ht) }
            0x2516 -> { vLine(canvas, top, cy, cx, ht); hLine(canvas, cx, right, cy, lt) }
            0x2517 -> { vLine(canvas, top, cy, cx, ht); hLine(canvas, cx, right, cy, ht) } // ┗
            0x2518 -> { vLine(canvas, top, cy, cx, lt); hLine(canvas, left, cx, cy, lt) } // ┘
            0x2519 -> { vLine(canvas, top, cy, cx, lt); hLine(canvas, left, cx, cy, ht) }
            0x251A -> { vLine(canvas, top, cy, cx, ht); hLine(canvas, left, cx, cy, lt) }
            0x251B -> { vLine(canvas, top, cy, cx, ht); hLine(canvas, left, cx, cy, ht) } // ┛
            // Light/heavy T-junctions
            0x251C -> { vLine(canvas, top, bottom, cx, lt); hLine(canvas, cx, right, cy, lt) } // ├
            0x251D -> { vLine(canvas, top, bottom, cx, lt); hLine(canvas, cx, right, cy, ht) }
            0x251E -> { vLine(canvas, top, cy, cx, ht); vLine(canvas, cy, bottom, cx, lt); hLine(canvas, cx, right, cy, lt) }
            0x251F -> { vLine(canvas, top, cy, cx, lt); vLine(canvas, cy, bottom, cx, ht); hLine(canvas, cx, right, cy, lt) }
            0x2520 -> { vLine(canvas, top, bottom, cx, ht); hLine(canvas, cx, right, cy, lt) }
            0x2521 -> { vLine(canvas, top, cy, cx, ht); vLine(canvas, cy, bottom, cx, lt); hLine(canvas, cx, right, cy, ht) }
            0x2522 -> { vLine(canvas, top, cy, cx, lt); vLine(canvas, cy, bottom, cx, ht); hLine(canvas, cx, right, cy, ht) }
            0x2523 -> { vLine(canvas, top, bottom, cx, ht); hLine(canvas, cx, right, cy, ht) } // ┣
            0x2524 -> { vLine(canvas, top, bottom, cx, lt); hLine(canvas, left, cx, cy, lt) } // ┤
            0x2525 -> { vLine(canvas, top, bottom, cx, lt); hLine(canvas, left, cx, cy, ht) }
            0x2526 -> { vLine(canvas, top, cy, cx, ht); vLine(canvas, cy, bottom, cx, lt); hLine(canvas, left, cx, cy, lt) }
            0x2527 -> { vLine(canvas, top, cy, cx, lt); vLine(canvas, cy, bottom, cx, ht); hLine(canvas, left, cx, cy, lt) }
            0x2528 -> { vLine(canvas, top, bottom, cx, ht); hLine(canvas, left, cx, cy, lt) }
            0x2529 -> { vLine(canvas, top, cy, cx, ht); vLine(canvas, cy, bottom, cx, lt); hLine(canvas, left, cx, cy, ht) }
            0x252A -> { vLine(canvas, top, cy, cx, lt); vLine(canvas, cy, bottom, cx, ht); hLine(canvas, left, cx, cy, ht) }
            0x252B -> { vLine(canvas, top, bottom, cx, ht); hLine(canvas, left, cx, cy, ht) } // ┫
            0x252C -> { hLine(canvas, left, right, cy, lt); vLine(canvas, cy, bottom, cx, lt) } // ┬
            0x252D -> { hLine(canvas, left, cx, cy, ht); hLine(canvas, cx, right, cy, lt); vLine(canvas, cy, bottom, cx, lt) }
            0x252E -> { hLine(canvas, left, cx, cy, lt); hLine(canvas, cx, right, cy, ht); vLine(canvas, cy, bottom, cx, lt) }
            0x252F -> { hLine(canvas, left, right, cy, ht); vLine(canvas, cy, bottom, cx, lt) }
            0x2530 -> { hLine(canvas, left, right, cy, lt); vLine(canvas, cy, bottom, cx, ht) }
            0x2531 -> { hLine(canvas, left, cx, cy, ht); hLine(canvas, cx, right, cy, lt); vLine(canvas, cy, bottom, cx, ht) }
            0x2532 -> { hLine(canvas, left, cx, cy, lt); hLine(canvas, cx, right, cy, ht); vLine(canvas, cy, bottom, cx, ht) }
            0x2533 -> { hLine(canvas, left, right, cy, ht); vLine(canvas, cy, bottom, cx, ht) } // ┳
            0x2534 -> { hLine(canvas, left, right, cy, lt); vLine(canvas, top, cy, cx, lt) } // ┴
            0x2535 -> { hLine(canvas, left, cx, cy, ht); hLine(canvas, cx, right, cy, lt); vLine(canvas, top, cy, cx, lt) }
            0x2536 -> { hLine(canvas, left, cx, cy, lt); hLine(canvas, cx, right, cy, ht); vLine(canvas, top, cy, cx, lt) }
            0x2537 -> { hLine(canvas, left, right, cy, ht); vLine(canvas, top, cy, cx, lt) }
            0x2538 -> { hLine(canvas, left, right, cy, lt); vLine(canvas, top, cy, cx, ht) }
            0x2539 -> { hLine(canvas, left, cx, cy, ht); hLine(canvas, cx, right, cy, lt); vLine(canvas, top, cy, cx, ht) }
            0x253A -> { hLine(canvas, left, cx, cy, lt); hLine(canvas, cx, right, cy, ht); vLine(canvas, top, cy, cx, ht) }
            0x253B -> { hLine(canvas, left, right, cy, ht); vLine(canvas, top, cy, cx, ht) } // ┻
            // Crosses
            0x253C -> { hLine(canvas, left, right, cy, lt); vLine(canvas, top, bottom, cx, lt) } // ┼
            0x253D -> { hLine(canvas, left, cx, cy, ht); hLine(canvas, cx, right, cy, lt); vLine(canvas, top, bottom, cx, lt) }
            0x253E -> { hLine(canvas, left, cx, cy, lt); hLine(canvas, cx, right, cy, ht); vLine(canvas, top, bottom, cx, lt) }
            0x253F -> { hLine(canvas, left, right, cy, ht); vLine(canvas, top, bottom, cx, lt) }
            0x2540 -> { hLine(canvas, left, right, cy, lt); vLine(canvas, top, cy, cx, ht); vLine(canvas, cy, bottom, cx, lt) }
            0x2541 -> { hLine(canvas, left, right, cy, lt); vLine(canvas, top, cy, cx, lt); vLine(canvas, cy, bottom, cx, ht) }
            0x2542 -> { hLine(canvas, left, right, cy, lt); vLine(canvas, top, bottom, cx, ht) }
            0x2543 -> { hLine(canvas, left, cx, cy, ht); hLine(canvas, cx, right, cy, lt); vLine(canvas, top, cy, cx, ht); vLine(canvas, cy, bottom, cx, lt) }
            0x2544 -> { hLine(canvas, left, cx, cy, lt); hLine(canvas, cx, right, cy, ht); vLine(canvas, top, cy, cx, ht); vLine(canvas, cy, bottom, cx, lt) }
            0x2545 -> { hLine(canvas, left, cx, cy, ht); hLine(canvas, cx, right, cy, lt); vLine(canvas, top, cy, cx, lt); vLine(canvas, cy, bottom, cx, ht) }
            0x2546 -> { hLine(canvas, left, cx, cy, lt); hLine(canvas, cx, right, cy, ht); vLine(canvas, top, cy, cx, lt); vLine(canvas, cy, bottom, cx, ht) }
            0x2547 -> { hLine(canvas, left, right, cy, ht); vLine(canvas, top, cy, cx, ht); vLine(canvas, cy, bottom, cx, lt) }
            0x2548 -> { hLine(canvas, left, right, cy, ht); vLine(canvas, top, cy, cx, lt); vLine(canvas, cy, bottom, cx, ht) }
            0x2549 -> { hLine(canvas, left, cx, cy, ht); hLine(canvas, cx, right, cy, lt); vLine(canvas, top, bottom, cx, ht) }
            0x254A -> { hLine(canvas, left, cx, cy, lt); hLine(canvas, cx, right, cy, ht); vLine(canvas, top, bottom, cx, ht) }
            0x254B -> { hLine(canvas, left, right, cy, ht); vLine(canvas, top, bottom, cx, ht) } // ╋
            // Double-dash
            0x254C -> hDash(canvas, left, right, cy, lt, 2) // ╌
            0x254D -> hDash(canvas, left, right, cy, ht, 2) // ╍
            0x254E -> vDash(canvas, top, bottom, cx, lt, 2) // ╎
            0x254F -> vDash(canvas, top, bottom, cx, ht, 2) // ╏
            // Double lines
            0x2550 -> doubleH(canvas, left, right, cy, lt) // ═
            0x2551 -> doubleV(canvas, top, bottom, cx, lt) // ║
            0x2552 -> { // ╒
                doubleH(canvas, cx, right, cy, lt)
                vLine(canvas, cy, bottom, cx, lt)
            }
            0x2553 -> { // ╓
                hLine(canvas, cx, right, cy, lt)
                doubleV(canvas, cy, bottom, cx, lt)
            }
            0x2554 -> { // ╔
                doubleH(canvas, cx, right, cy, lt)
                doubleV(canvas, cy, bottom, cx, lt)
                // close the corner joint
                hLine(canvas, cx - lt, cx + lt, cy - lt * 1.5f, lt)
            }
            0x2555 -> { // ╕
                doubleH(canvas, left, cx, cy, lt)
                vLine(canvas, cy, bottom, cx, lt)
            }
            0x2556 -> { // ╖
                hLine(canvas, left, cx, cy, lt)
                doubleV(canvas, cy, bottom, cx, lt)
            }
            0x2557 -> { // ╗
                doubleH(canvas, left, cx, cy, lt)
                doubleV(canvas, cy, bottom, cx, lt)
                hLine(canvas, cx - lt, cx + lt, cy - lt * 1.5f, lt)
            }
            0x2558 -> { // ╘
                doubleH(canvas, cx, right, cy, lt)
                vLine(canvas, top, cy, cx, lt)
            }
            0x2559 -> { // ╙
                hLine(canvas, cx, right, cy, lt)
                doubleV(canvas, top, cy, cx, lt)
            }
            0x255A -> { // ╚
                doubleH(canvas, cx, right, cy, lt)
                doubleV(canvas, top, cy, cx, lt)
                hLine(canvas, cx - lt, cx + lt, cy + lt * 1.5f, lt)
            }
            0x255B -> { // ╛
                doubleH(canvas, left, cx, cy, lt)
                vLine(canvas, top, cy, cx, lt)
            }
            0x255C -> { // ╜
                hLine(canvas, left, cx, cy, lt)
                doubleV(canvas, top, cy, cx, lt)
            }
            0x255D -> { // ╝
                doubleH(canvas, left, cx, cy, lt)
                doubleV(canvas, top, cy, cx, lt)
                hLine(canvas, cx - lt, cx + lt, cy + lt * 1.5f, lt)
            }
            0x255E -> { // ╞
                doubleH(canvas, cx, right, cy, lt)
                vLine(canvas, top, bottom, cx, lt)
            }
            0x255F -> { // ╟
                hLine(canvas, cx, right, cy, lt)
                doubleV(canvas, top, bottom, cx, lt)
            }
            0x2560 -> { // ╠
                doubleH(canvas, cx, right, cy, lt)
                doubleV(canvas, top, bottom, cx, lt)
            }
            0x2561 -> { // ╡
                doubleH(canvas, left, cx, cy, lt)
                vLine(canvas, top, bottom, cx, lt)
            }
            0x2562 -> { // ╢
                hLine(canvas, left, cx, cy, lt)
                doubleV(canvas, top, bottom, cx, lt)
            }
            0x2563 -> { // ╣
                doubleH(canvas, left, cx, cy, lt)
                doubleV(canvas, top, bottom, cx, lt)
            }
            0x2564 -> { // ╤
                doubleH(canvas, left, right, cy, lt)
                vLine(canvas, cy, bottom, cx, lt)
            }
            0x2565 -> { // ╥
                hLine(canvas, left, right, cy, lt)
                doubleV(canvas, cy, bottom, cx, lt)
            }
            0x2566 -> { // ╦
                doubleH(canvas, left, right, cy, lt)
                doubleV(canvas, cy, bottom, cx, lt)
            }
            0x2567 -> { // ╧
                doubleH(canvas, left, right, cy, lt)
                vLine(canvas, top, cy, cx, lt)
            }
            0x2568 -> { // ╨
                hLine(canvas, left, right, cy, lt)
                doubleV(canvas, top, cy, cx, lt)
            }
            0x2569 -> { // ╩
                doubleH(canvas, left, right, cy, lt)
                doubleV(canvas, top, cy, cx, lt)
            }
            0x256A -> { // ╪
                doubleH(canvas, left, right, cy, lt)
                vLine(canvas, top, bottom, cx, lt)
            }
            0x256B -> { // ╫
                hLine(canvas, left, right, cy, lt)
                doubleV(canvas, top, bottom, cx, lt)
            }
            0x256C -> { // ╬
                doubleH(canvas, left, right, cy, lt)
                doubleV(canvas, top, bottom, cx, lt)
            }
            // Rounded corners — same stubs as ┌┐└┘ so they meet │/─ edge-to-edge.
            // (Arc-only drawing left gaps against neighboring cells.)
            0x256D -> { // ╭
                vLine(canvas, cy, bottom, cx, lt)
                hLine(canvas, cx, right, cy, lt)
            }
            0x256E -> { // ╮
                vLine(canvas, cy, bottom, cx, lt)
                hLine(canvas, left, cx, cy, lt)
            }
            0x256F -> { // ╯
                vLine(canvas, top, cy, cx, lt)
                hLine(canvas, left, cx, cy, lt)
            }
            0x2570 -> { // ╰
                vLine(canvas, top, cy, cx, lt)
                hLine(canvas, cx, right, cy, lt)
            }
            // Diagonals
            0x2571 -> diag(canvas, left, bottom, right, top, lt) // ╱
            0x2572 -> diag(canvas, left, top, right, bottom, lt) // ╲
            0x2573 -> { // ╳
                diag(canvas, left, bottom, right, top, lt)
                diag(canvas, left, top, right, bottom, lt)
            }
            // Half lines from center
            0x2574 -> hLine(canvas, left, cx, cy, lt) // ╴
            0x2575 -> vLine(canvas, top, cy, cx, lt) // ╵
            0x2576 -> hLine(canvas, cx, right, cy, lt) // ╶
            0x2577 -> vLine(canvas, cy, bottom, cx, lt) // ╷
            0x2578 -> hLine(canvas, left, cx, cy, ht) // ╸
            0x2579 -> vLine(canvas, top, cy, cx, ht) // ╹
            0x257A -> hLine(canvas, cx, right, cy, ht) // ╺
            0x257B -> vLine(canvas, cy, bottom, cx, ht) // ╻
            0x257C -> { hLine(canvas, left, cx, cy, lt); hLine(canvas, cx, right, cy, ht) } // ╼
            0x257D -> { vLine(canvas, top, cy, cx, lt); vLine(canvas, cy, bottom, cx, ht) } // ╽
            0x257E -> { hLine(canvas, left, cx, cy, ht); hLine(canvas, cx, right, cy, lt) } // ╾
            0x257F -> { vLine(canvas, top, cy, cx, ht); vLine(canvas, cy, bottom, cx, lt) } // ╿
            else -> hLine(canvas, left, right, cy, lt)
        }
    }

    /** Extend past cell edges so adjacent │/─/corners fuse into one stroke. */
    private fun hLine(canvas: Canvas, x0: Float, x1: Float, y: Float, t: Float) {
        val a = minOf(x0, x1) - EDGE
        val b = maxOf(x0, x1) + EDGE
        val half = t * 0.5f
        canvas.drawRect(a, y - half, b, y + half, fill)
    }

    private fun vLine(canvas: Canvas, y0: Float, y1: Float, x: Float, t: Float) {
        val a = minOf(y0, y1) - EDGE
        val b = maxOf(y0, y1) + EDGE
        val half = t * 0.5f
        canvas.drawRect(x - half, a, x + half, b, fill)
    }

    private fun doubleH(canvas: Canvas, x0: Float, x1: Float, y: Float, t: Float) {
        val gap = t * 1.6f
        hLine(canvas, x0, x1, y - gap, t)
        hLine(canvas, x0, x1, y + gap, t)
    }

    private fun doubleV(canvas: Canvas, y0: Float, y1: Float, x: Float, t: Float) {
        val gap = t * 1.6f
        vLine(canvas, y0, y1, x - gap, t)
        vLine(canvas, y0, y1, x + gap, t)
    }

    private fun hDash(canvas: Canvas, x0: Float, x1: Float, y: Float, t: Float, parts: Int) {
        val span = x1 - x0
        val seg = span / (parts * 2f - 1f)
        var i = 0
        while (i < parts) {
            val a = x0 + i * 2f * seg
            hLine(canvas, a, a + seg, y, t)
            i++
        }
    }

    private fun vDash(canvas: Canvas, y0: Float, y1: Float, x: Float, t: Float, parts: Int) {
        val span = y1 - y0
        val seg = span / (parts * 2f - 1f)
        var i = 0
        while (i < parts) {
            val a = y0 + i * 2f * seg
            vLine(canvas, a, a + seg, x, t)
            i++
        }
    }

    private fun diag(canvas: Canvas, x0: Float, y0: Float, x1: Float, y1: Float, t: Float) {
        stroke.strokeWidth = t
        canvas.drawLine(x0, y0, x1, y1, stroke)
    }

    // --- Block elements ---

    private fun drawBlock(
        canvas: Canvas,
        cp: Int,
        left: Float,
        top: Float,
        width: Float,
        height: Float,
        fg: Int,
    ) {
        fill.color = fg
        val right = left + width
        val bottom = top + height
        val cx = left + width * 0.5f
        val cy = top + height * 0.5f
        when (cp) {
            0x2580 -> canvas.drawRect(left, top, right, cy, fill) // ▀
            0x2581 -> canvas.drawRect(left, bottom - height * 0.125f, right, bottom, fill) // ▁
            0x2582 -> canvas.drawRect(left, bottom - height * 0.25f, right, bottom, fill)
            0x2583 -> canvas.drawRect(left, bottom - height * 0.375f, right, bottom, fill)
            0x2584 -> canvas.drawRect(left, cy, right, bottom, fill) // ▄
            0x2585 -> canvas.drawRect(left, bottom - height * 0.625f, right, bottom, fill)
            0x2586 -> canvas.drawRect(left, bottom - height * 0.75f, right, bottom, fill)
            0x2587 -> canvas.drawRect(left, bottom - height * 0.875f, right, bottom, fill)
            0x2588 -> canvas.drawRect(left, top, right, bottom, fill) // █
            0x2589 -> canvas.drawRect(left, top, left + width * 0.875f, bottom, fill)
            0x258A -> canvas.drawRect(left, top, left + width * 0.75f, bottom, fill)
            0x258B -> canvas.drawRect(left, top, left + width * 0.625f, bottom, fill)
            0x258C -> canvas.drawRect(left, top, cx, bottom, fill) // ▌
            0x258D -> canvas.drawRect(left, top, left + width * 0.375f, bottom, fill)
            0x258E -> canvas.drawRect(left, top, left + width * 0.25f, bottom, fill)
            0x258F -> canvas.drawRect(left, top, left + width * 0.125f, bottom, fill) // ▏
            0x2590 -> canvas.drawRect(cx, top, right, bottom, fill) // ▐
            0x2591 -> { // ░
                fill.color = (fg and 0x00FFFFFF) or (0x40 shl 24)
                canvas.drawRect(left, top, right, bottom, fill)
                fill.color = fg
            }
            0x2592 -> { // ▒
                fill.color = (fg and 0x00FFFFFF) or (0x80 shl 24)
                canvas.drawRect(left, top, right, bottom, fill)
                fill.color = fg
            }
            0x2593 -> { // ▓
                fill.color = (fg and 0x00FFFFFF) or (0xC0 shl 24)
                canvas.drawRect(left, top, right, bottom, fill)
                fill.color = fg
            }
            0x2594 -> canvas.drawRect(left, top, right, top + height * 0.125f, fill) // ▔
            0x2595 -> canvas.drawRect(right - width * 0.125f, top, right, bottom, fill) // ▕
            0x2596 -> canvas.drawRect(left, cy, cx, bottom, fill) // ▖
            0x2597 -> canvas.drawRect(cx, cy, right, bottom, fill) // ▗
            0x2598 -> canvas.drawRect(left, top, cx, cy, fill) // ▘
            0x2599 -> { // ▙
                canvas.drawRect(left, top, cx, bottom, fill)
                canvas.drawRect(cx, cy, right, bottom, fill)
            }
            0x259A -> { // ▚
                canvas.drawRect(left, top, cx, cy, fill)
                canvas.drawRect(cx, cy, right, bottom, fill)
            }
            0x259B -> { // ▛
                canvas.drawRect(left, top, right, cy, fill)
                canvas.drawRect(left, cy, cx, bottom, fill)
            }
            0x259C -> { // ▜
                canvas.drawRect(left, top, right, cy, fill)
                canvas.drawRect(cx, cy, right, bottom, fill)
            }
            0x259D -> canvas.drawRect(cx, top, right, cy, fill) // ▝
            0x259E -> { // ▞
                canvas.drawRect(cx, top, right, cy, fill)
                canvas.drawRect(left, cy, cx, bottom, fill)
            }
            0x259F -> { // ▟
                canvas.drawRect(cx, top, right, bottom, fill)
                canvas.drawRect(left, cy, cx, bottom, fill)
            }
        }
    }

    // --- Powerline ---

    private fun drawPowerline(
        canvas: Canvas,
        cp: Int,
        left: Float,
        top: Float,
        width: Float,
        height: Float,
        fg: Int,
        bg: Int,
    ) {
        // Always paint cell bg — separators are two-tone.
        fill.color = bg
        canvas.drawRect(left, top, left + width, top + height, fill)
        fillAA.color = fg
        stroke.color = fg
        stroke.strokeWidth = maxOf(1f, minOf(width, height) / 10f)
        val right = left + width
        val bottom = top + height
        val midY = top + height * 0.5f
        path.reset()
        when (cp) {
            0xE0B0 -> { // solid right triangle
                path.moveTo(left, top)
                path.lineTo(right, midY)
                path.lineTo(left, bottom)
                path.close()
                canvas.drawPath(path, fillAA)
            }
            0xE0B1 -> { // hollow right
                path.moveTo(left, top)
                path.lineTo(right, midY)
                path.lineTo(left, bottom)
                canvas.drawPath(path, stroke)
            }
            0xE0B2 -> { // solid left
                path.moveTo(right, top)
                path.lineTo(left, midY)
                path.lineTo(right, bottom)
                path.close()
                canvas.drawPath(path, fillAA)
            }
            0xE0B3 -> {
                path.moveTo(right, top)
                path.lineTo(left, midY)
                path.lineTo(right, bottom)
                canvas.drawPath(path, stroke)
            }
            0xE0B4 -> { // right semicircle
                oval.set(left - width, top, left + width, bottom)
                canvas.drawArc(oval, -90f, 180f, true, fillAA)
            }
            0xE0B5 -> {
                oval.set(left - width, top, left + width, bottom)
                canvas.drawArc(oval, -90f, 180f, false, stroke)
            }
            0xE0B6 -> {
                oval.set(left, top, left + width * 2f, bottom)
                canvas.drawArc(oval, 90f, 180f, true, fillAA)
            }
            0xE0B7 -> {
                oval.set(left, top, left + width * 2f, bottom)
                canvas.drawArc(oval, 90f, 180f, false, stroke)
            }
            0xE0B8 -> { // bottom-left triangle
                path.moveTo(left, top)
                path.lineTo(right, top)
                path.lineTo(left, bottom)
                path.close()
                canvas.drawPath(path, fillAA)
            }
            0xE0B9 -> {
                path.moveTo(left, top)
                path.lineTo(right, top)
                path.lineTo(left, bottom)
                canvas.drawPath(path, stroke)
            }
            0xE0BA -> {
                path.moveTo(left, top)
                path.lineTo(right, top)
                path.lineTo(right, bottom)
                path.close()
                canvas.drawPath(path, fillAA)
            }
            0xE0BB -> {
                path.moveTo(left, top)
                path.lineTo(right, top)
                path.lineTo(right, bottom)
                canvas.drawPath(path, stroke)
            }
            0xE0BC -> {
                path.moveTo(left, top)
                path.lineTo(right, bottom)
                path.lineTo(left, bottom)
                path.close()
                canvas.drawPath(path, fillAA)
            }
            0xE0BD -> {
                path.moveTo(left, top)
                path.lineTo(right, bottom)
                path.lineTo(left, bottom)
                canvas.drawPath(path, stroke)
            }
            0xE0BE -> {
                path.moveTo(right, top)
                path.lineTo(right, bottom)
                path.lineTo(left, bottom)
                path.close()
                canvas.drawPath(path, fillAA)
            }
            0xE0BF -> {
                path.moveTo(right, top)
                path.lineTo(right, bottom)
                path.lineTo(left, bottom)
                canvas.drawPath(path, stroke)
            }
        }
    }

    // --- Prompt / geometric arrows ---
    // Only for glyphs JetBrains Mono lacks. Sized like a font arrow (~55% cell),
    // not the old edge-to-edge strokes.

    private fun drawArrow(
        canvas: Canvas,
        cp: Int,
        left: Float,
        top: Float,
        width: Float,
        height: Float,
        fg: Int,
    ) {
        fillAA.color = fg
        stroke.color = fg
        stroke.strokeCap = Paint.Cap.ROUND
        stroke.strokeJoin = Paint.Join.ROUND
        val cx = left + width * 0.5f
        val cy = top + height * 0.5f
        val midY = cy - height * 0.02f
        val halfH = height * 0.28f
        val halfW = width * 0.32f
        path.reset()
        when (cp) {
            0x23F5 -> { // ⏵ — compact filled triangle (Claude caret)
                path.moveTo(cx - halfW * 0.45f, midY - halfH)
                path.lineTo(cx + halfW * 0.70f, midY)
                path.lineTo(cx - halfW * 0.45f, midY + halfH)
                path.close()
                canvas.drawPath(path, fillAA)
            }
            0x23F4 -> {
                path.moveTo(cx + halfW * 0.45f, midY - halfH)
                path.lineTo(cx - halfW * 0.70f, midY)
                path.lineTo(cx + halfW * 0.45f, midY + halfH)
                path.close()
                canvas.drawPath(path, fillAA)
            }
            0x23F6 -> {
                path.moveTo(cx, midY - halfH)
                path.lineTo(cx + halfW, midY + halfH * 0.75f)
                path.lineTo(cx - halfW, midY + halfH * 0.75f)
                path.close()
                canvas.drawPath(path, fillAA)
            }
            0x23F7 -> {
                path.moveTo(cx - halfW, midY - halfH * 0.75f)
                path.lineTo(cx + halfW, midY - halfH * 0.75f)
                path.lineTo(cx, midY + halfH)
                path.close()
                canvas.drawPath(path, fillAA)
            }
            0x27A4 -> { // ➤ — match JBM → proportions
                val tipX = cx + halfW
                val tailX = cx - halfW * 0.85f
                val wing = halfH * 0.42f
                val head = halfW * 0.48f
                stroke.strokeWidth = maxOf(1.75f, width * 0.11f)
                canvas.drawLine(tailX, midY, tipX - head * 0.2f, midY, stroke)
                path.moveTo(tipX - head, midY - wing)
                path.lineTo(tipX, midY)
                path.lineTo(tipX - head, midY + wing)
                canvas.drawPath(path, stroke)
            }
            else -> Unit
        }
        stroke.strokeCap = Paint.Cap.BUTT
        stroke.strokeJoin = Paint.Join.MITER
    }
}
