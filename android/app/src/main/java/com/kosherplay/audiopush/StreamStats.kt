package com.kosherplay.audiopush

import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicInteger
import java.util.concurrent.atomic.AtomicLong

/**
 * Counters shared between the worker threads and the UI. The UI only ever
 * reads, so drawing can never stall audio.
 */
object StreamStats {
    val blocks = AtomicLong(0)
    val lost = AtomicLong(0)
    val dropouts = AtomicLong(0)
    val late = AtomicLong(0)
    val bufferBlocks = AtomicInteger(0)
    val targetBlocks = AtomicInteger(0)
    val lastPacketMs = AtomicLong(0)
    val running = AtomicBoolean(false)

    /** Peak level of the last block, 0..1, stored as float bits. */
    private val levelBits = AtomicInteger(0)

    var peerText: String? = null
        @Synchronized get
        @Synchronized set

    var errorText: String? = null
        @Synchronized get
        @Synchronized set

    /** Where the platform says playback is actually going right now. */
    var outputText: String? = null
        @Synchronized get
        @Synchronized set

    fun setLevel(value: Float) = levelBits.set(java.lang.Float.floatToRawIntBits(value))

    fun level(): Float = java.lang.Float.intBitsToFloat(levelBits.get())

    fun streamLive(): Boolean {
        val last = lastPacketMs.get()
        return last != 0L && System.currentTimeMillis() - last < Config.STREAM_TIMEOUT_MS
    }

    fun reset() {
        blocks.set(0)
        lost.set(0)
        dropouts.set(0)
        late.set(0)
        bufferBlocks.set(0)
        targetBlocks.set(0)
        lastPacketMs.set(0)
        setLevel(0f)
        peerText = null
        errorText = null
        outputText = null
    }
}
