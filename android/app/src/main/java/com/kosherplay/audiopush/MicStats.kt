package com.kosherplay.audiopush

import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicInteger
import java.util.concurrent.atomic.AtomicLong

/**
 * Counters for the return microphone stream, kept apart from the main ones so
 * that one stream failing cannot blank the other's readout.
 */
object MicStats {
    val blocks = AtomicLong(0)
    val lastPacketMs = AtomicLong(0)
    val running = AtomicBoolean(false)

    private val levelBits = AtomicInteger(0)

    var errorText: String? = null
        @Synchronized get
        @Synchronized set

    /**
     * Where the microphone is actually going, latched for the session.
     * Shown in the UI: a microphone whose destination is invisible is a
     * microphone you cannot trust.
     */
    var targetText: String? = null
        @Synchronized get
        @Synchronized set

    fun setLevel(value: Float) = levelBits.set(java.lang.Float.floatToRawIntBits(value))

    fun level(): Float = java.lang.Float.intBitsToFloat(levelBits.get())

    fun reset() {
        blocks.set(0)
        lastPacketMs.set(0)
        setLevel(0f)
        errorText = null
        targetText = null
    }
}
