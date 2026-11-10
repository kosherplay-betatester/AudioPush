package com.kosherplay.audiopush

import android.media.AudioAttributes
import android.media.AudioFormat
import android.media.AudioManager
import android.media.AudioTrack
import android.os.Build
import android.os.Process
import java.net.DatagramPacket
import java.net.DatagramSocket
import java.net.InetSocketAddress
import java.util.concurrent.atomic.AtomicBoolean

/**
 * Phone as speaker: UDP in, jitter buffer, AudioTrack out.
 *
 * Mirrors the Windows receiver exactly, including the adaptive buffer depth,
 * the one-frame clock drift correction and the fade-out concealment. The
 * only structural difference is that `AudioTrack.write` blocks, so it paces
 * the loop in place of a buffer-ready event.
 */
class ReceiverEngine(private val preset: Latency) {
    private val stop = AtomicBoolean(false)
    private val ring = BlockRing(Config.RING_BLOCKS)
    private var netThread: Thread? = null
    private var playThread: Thread? = null

    fun start() {
        stop.set(false)
        StreamStats.reset()
        StreamStats.running.set(true)
        netThread = Thread({ receiveLoop() }, "audiopush-udp-recv").apply {
            isDaemon = true
            start()
        }
        playThread = Thread({ playLoop() }, "audiopush-play").apply {
            isDaemon = true
            start()
        }
    }

    fun stop() {
        stop.set(true)
        netThread?.join(800)
        playThread?.join(800)
        netThread = null
        playThread = null
        StreamStats.running.set(false)
    }

    // ---------------------------------------------------------------- network

    private fun receiveLoop() {
        var socket: DatagramSocket? = null
        try {
            socket = DatagramSocket(null).apply {
                reuseAddress = true
                bind(InetSocketAddress(Config.AUDIO_PORT))
                soTimeout = 100
                // A roomy kernel buffer: a scheduling hiccup in the network
                // stack then costs us nothing, where a small one costs packets.
                receiveBufferSize = Protocol.PACKET_LEN * 256
            }
            val buffer = ByteArray(Protocol.PACKET_LEN + 64)
            val block = ShortArray(Config.BLOCK_SAMPLES)
            var expectedSeq: Int? = null
            var peer: String? = null

            while (!stop.get()) {
                val packet = DatagramPacket(buffer, buffer.size)
                try {
                    socket.receive(packet)
                } catch (_: Exception) {
                    continue // timeout; re-check the stop flag
                }

                val seq = Protocol.decodeAudioSeq(packet.data, packet.length) ?: continue

                expectedSeq?.let { expected ->
                    val gap = seq - expected
                    // A huge gap means the sender restarted, not that many
                    // packets were lost, so only plausible losses are counted.
                    if (gap in 1..999) StreamStats.lost.addAndGet(gap.toLong())
                }
                expectedSeq = seq + 1

                val peerText = "${packet.address.hostAddress}:${packet.port}"
                if (peer != peerText) {
                    peer = peerText
                    StreamStats.peerText = peerText
                }

                Protocol.payloadIntoBlock(packet.data, block)
                if (!ring.push(block)) StreamStats.late.incrementAndGet()
                StreamStats.blocks.incrementAndGet()
                StreamStats.lastPacketMs.set(System.currentTimeMillis())
                StreamStats.bufferBlocks.set(ring.size())
            }
        } catch (e: Exception) {
            StreamStats.errorText = "Could not listen on port ${Config.AUDIO_PORT}: ${e.message}"
        } finally {
            socket?.close()
        }
    }

    // --------------------------------------------------------------- playback

    private fun playLoop() {
        Process.setThreadPriority(Process.THREAD_PRIORITY_URGENT_AUDIO)

        val minBytes = AudioTrack.getMinBufferSize(
            Config.SAMPLE_RATE,
            AudioFormat.CHANNEL_OUT_STEREO,
            AudioFormat.ENCODING_PCM_16BIT
        )
        if (minBytes <= 0) {
            StreamStats.errorText = "This device rejected 48 kHz stereo playback."
            return
        }
        val deviceBufferFrames = minBytes / (Config.CHANNELS * 2)

        val track = buildTrack(minBytes) ?: return

        // The preset is the opening depth; from here the buffer adapts.
        val baseTarget = preset.targetBlocks(deviceBufferFrames)
        val floorTarget = maxOf(
            2,
            (deviceBufferFrames + Config.FRAMES_PER_BLOCK - 1) / Config.FRAMES_PER_BLOCK + 1
        )
        val maxTarget = baseTarget * 2
        var target = baseTarget
        var ceiling = target * 2

        val block = ShortArray(Config.BLOCK_SAMPLES)
        val conceal = ShortArray(Config.BLOCK_SAMPLES)
        var lastLeft = 0.toShort()
        var lastRight = 0.toShort()
        var primed = false
        var lastTrouble = System.currentTimeMillis()
        var lastAdapt = System.currentTimeMillis()
        var minDepth = Int.MAX_VALUE

        try {
            track.play()
            while (!stop.get()) {
                val depth = ring.size()
                StreamStats.bufferBlocks.set(depth)
                StreamStats.targetBlocks.set(target)

                if (depth > ceiling) {
                    StreamStats.late.addAndGet(ring.trimTo(target).toLong())
                    // Overshoot is trouble too: it means delivery is bursty.
                    lastTrouble = System.currentTimeMillis()
                }

                // Adaptive depth: give a block back after a quiet spell, but
                // only if the low-water mark says the margin was truly spare.
                minDepth = minOf(minDepth, depth)
                val now = System.currentTimeMillis()
                if (now - lastAdapt > 5000) {
                    lastAdapt = now
                    if (now - lastTrouble > 12_000 && minDepth >= 2 && target > floorTarget) {
                        target--
                        ceiling = target * 2
                    }
                    minDepth = Int.MAX_VALUE
                }

                if (!primed) {
                    if (depth >= target) {
                        primed = true
                    } else {
                        // Hold silence until the buffer has filled.
                        java.util.Arrays.fill(conceal, 0)
                        track.write(conceal, 0, Config.BLOCK_SAMPLES)
                        StreamStats.setLevel(0f)
                        continue
                    }
                }

                if (!ring.pop(block)) {
                    // Ran dry. Fade the last frame out rather than cutting to
                    // zero: the click you hear is the edge in the waveform.
                    var decay = 1f
                    var i = 0
                    while (i < Config.BLOCK_SAMPLES) {
                        conceal[i] = (lastLeft * decay).toInt().toShort()
                        conceal[i + 1] = (lastRight * decay).toInt().toShort()
                        decay *= 0.98f
                        i += 2
                    }
                    lastLeft = 0
                    lastRight = 0
                    track.write(conceal, 0, Config.BLOCK_SAMPLES)
                    StreamStats.dropouts.incrementAndGet()
                    // Slow to shrink, fast to grow.
                    target = minOf(target + 2, maxTarget)
                    ceiling = target * 2
                    lastTrouble = System.currentTimeMillis()
                    minDepth = Int.MAX_VALUE
                    primed = false
                    continue
                }

                var peak = 0f
                for (sample in block) {
                    val magnitude = kotlin.math.abs(sample.toInt()) / 32768f
                    if (magnitude > peak) peak = magnitude
                }
                StreamStats.setLevel(peak)
                lastLeft = block[Config.BLOCK_SAMPLES - 2]
                lastRight = block[Config.BLOCK_SAMPLES - 1]

                // Clock drift correction. The phone and the PC run off
                // independent crystals, so one produces audio a little faster
                // than the other consumes it. Writing one frame fewer or one
                // frame more (20 us) holds the buffer centred indefinitely
                // and is inaudible.
                when {
                    depth > target + 1 -> track.write(block, 0, Config.BLOCK_SAMPLES - 2)
                    depth + 1 < target -> {
                        track.write(block, 0, Config.BLOCK_SAMPLES)
                        track.write(block, Config.BLOCK_SAMPLES - 2, 2)
                    }
                    else -> track.write(block, 0, Config.BLOCK_SAMPLES)
                }
            }
        } catch (e: Exception) {
            StreamStats.errorText = "Playback failed: ${e.message}"
        } finally {
            runCatching { track.stop() }
            runCatching { track.release() }
        }
    }

    private fun buildTrack(minBytes: Int): AudioTrack? = try {
        val builder = AudioTrack.Builder()
            .setAudioAttributes(
                AudioAttributes.Builder()
                    .setUsage(AudioAttributes.USAGE_MEDIA)
                    .setContentType(AudioAttributes.CONTENT_TYPE_MUSIC)
                    .build()
            )
            .setAudioFormat(
                AudioFormat.Builder()
                    .setEncoding(AudioFormat.ENCODING_PCM_16BIT)
                    .setSampleRate(Config.SAMPLE_RATE)
                    .setChannelMask(AudioFormat.CHANNEL_OUT_STEREO)
                    .build()
            )
            .setBufferSizeInBytes(minBytes)
            .setTransferMode(AudioTrack.MODE_STREAM)
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            // Asks the platform for the fast mixer path.
            builder.setPerformanceMode(AudioTrack.PERFORMANCE_MODE_LOW_LATENCY)
        }
        builder.build()
    } catch (e: Exception) {
        StreamStats.errorText = "Could not open the speaker: ${e.message}"
        null
    }

    companion object {
        /** Sample rate the device prefers, for the diagnostics line. */
        fun nativeRate(audioManager: AudioManager): String =
            audioManager.getProperty(AudioManager.PROPERTY_OUTPUT_SAMPLE_RATE) ?: "unknown"
    }
}
