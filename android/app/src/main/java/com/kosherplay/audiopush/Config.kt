package com.kosherplay.audiopush

/**
 * Wire and buffering constants. Every value here must match the Windows
 * build byte for byte, or the two ends will not understand each other.
 */
object Config {
    const val SAMPLE_RATE = 48_000
    const val CHANNELS = 2

    /** 240 frames @ 48 kHz = 5.0 ms. One block per UDP datagram. */
    const val FRAMES_PER_BLOCK = 240
    const val BLOCK_SAMPLES = FRAMES_PER_BLOCK * CHANNELS // 480 shorts
    const val BLOCK_BYTES = BLOCK_SAMPLES * 2 // 960 bytes
    const val BLOCK_MS = FRAMES_PER_BLOCK * 1000f / SAMPLE_RATE

    const val AUDIO_PORT = 45778
    const val DISCOVERY_PORT = 45777

    /** Ring capacity in blocks; must be a power of two. 512 = 2.5 s. */
    const val RING_BLOCKS = 512

    const val ANNOUNCE_INTERVAL_MS = 1000L
    const val PEER_TIMEOUT_MS = 4000L
    const val STREAM_TIMEOUT_MS = 1000L
}

/**
 * Latency presets. The margin is buffering held on top of one AudioTrack
 * buffer; the receiver adapts downward from there while the link behaves.
 */
enum class Latency(val label: String, val marginMs: Float, val hint: String) {
    LOW("Low", 8f, "Least delay. Best over USB tethering or a strong 5 GHz link."),
    BALANCED("Balanced", 20f, "Starts safe, then tightens itself up. Recommended."),
    STABLE("Stable", 50f, "Most delay, rides out a weak or busy WiFi link.");

    /**
     * Opening jitter-buffer depth in blocks.
     *
     * This has to cover the AudioTrack buffer, not just the network: the
     * player hands the device a whole buffer at a time, so a target smaller
     * than that quantum runs dry on every write however good the link is.
     */
    fun targetBlocks(deviceBufferFrames: Int): Int {
        val marginFrames = (marginMs * Config.SAMPLE_RATE / 1000f).toInt()
        val frames = deviceBufferFrames + marginFrames
        return maxOf(2, (frames + Config.FRAMES_PER_BLOCK - 1) / Config.FRAMES_PER_BLOCK)
    }

    fun estimateMs(): Float = marginMs + Config.BLOCK_MS + 20f
}
