package com.kosherplay.audiopush

import android.media.AudioAttributes
import android.media.AudioFormat
import android.media.AudioPlaybackCaptureConfiguration
import android.media.AudioRecord
import android.media.projection.MediaProjection
import android.os.Build
import android.os.Process
import androidx.annotation.RequiresApi
import java.net.DatagramPacket
import java.net.DatagramSocket
import java.net.InetAddress
import java.util.concurrent.atomic.AtomicBoolean

/**
 * Phone as source: captures what other apps are playing and streams it to a
 * PC running AudioPush in receive mode.
 *
 * Android has no equivalent of WASAPI loopback. The nearest thing is
 * `AudioPlaybackCapture` (Android 10+), which needs the user to grant a
 * MediaProjection and which **individual apps are allowed to refuse**. An app
 * that sets `android:allowAudioPlaybackCapture="false"`, or whose audio is
 * flagged as media with capture disallowed, is silently skipped by the
 * platform - we receive silence for it, with no error. Most DRM-protected
 * video and several music services opt out. There is no way around that from
 * inside an app; it is a platform policy, not a limitation of this code.
 */
@RequiresApi(Build.VERSION_CODES.Q)
class SenderEngine(
    private val projection: MediaProjection,
    private val destination: InetAddress,
    private val destinationPort: Int,
    /** The pairing shared with that PC. */
    private val token: Token
) {
    private val stop = AtomicBoolean(false)
    private val ring = BlockRing(Config.RING_BLOCKS)
    private var captureThread: Thread? = null
    private var netThread: Thread? = null

    fun start() {
        stop.set(false)
        StreamStats.reset()
        StreamStats.running.set(true)
        StreamStats.peerText = "${destination.hostAddress}:$destinationPort"
        captureThread = Thread({ captureLoop() }, "audiopush-capture").apply {
            isDaemon = true
            start()
        }
        netThread = Thread({ sendLoop() }, "audiopush-udp-send").apply {
            isDaemon = true
            start()
        }
    }

    fun stop() {
        stop.set(true)
        captureThread?.join(800)
        netThread?.join(800)
        captureThread = null
        netThread = null
        runCatching { projection.stop() }
        StreamStats.running.set(false)
    }

    private fun captureLoop() {
        Process.setThreadPriority(Process.THREAD_PRIORITY_URGENT_AUDIO)

        val config = AudioPlaybackCaptureConfiguration.Builder(projection)
            .addMatchingUsage(AudioAttributes.USAGE_MEDIA)
            .addMatchingUsage(AudioAttributes.USAGE_GAME)
            .addMatchingUsage(AudioAttributes.USAGE_UNKNOWN)
            .build()

        val minBytes = AudioRecord.getMinBufferSize(
            Config.SAMPLE_RATE,
            AudioFormat.CHANNEL_IN_STEREO,
            AudioFormat.ENCODING_PCM_16BIT
        )
        if (minBytes <= 0) {
            StreamStats.errorText = "This device rejected 48 kHz stereo capture."
            return
        }

        val record = try {
            AudioRecord.Builder()
                .setAudioFormat(
                    AudioFormat.Builder()
                        .setEncoding(AudioFormat.ENCODING_PCM_16BIT)
                        .setSampleRate(Config.SAMPLE_RATE)
                        .setChannelMask(AudioFormat.CHANNEL_IN_STEREO)
                        .build()
                )
                .setBufferSizeInBytes(minBytes * 2)
                .setAudioPlaybackCaptureConfig(config)
                .build()
        } catch (e: Exception) {
            StreamStats.errorText = "Could not start capture: ${e.message}"
            return
        }

        // Pre-allocated; the loop never asks the allocator for anything.
        val scratch = ShortArray(Config.BLOCK_SAMPLES * 4)
        val block = ShortArray(Config.BLOCK_SAMPLES)
        var fill = 0
        var peak = 0f

        try {
            record.startRecording()
            StreamStats.errorText = null
            while (!stop.get()) {
                val read = record.read(scratch, 0, scratch.size)
                if (read <= 0) {
                    Thread.sleep(2)
                    continue
                }
                for (i in 0 until read) {
                    val sample = scratch[i]
                    val magnitude = kotlin.math.abs(sample.toInt()) / 32768f
                    if (magnitude > peak) peak = magnitude
                    block[fill++] = sample
                    if (fill == Config.BLOCK_SAMPLES) {
                        // A full ring means the network thread stalled; drop
                        // the block rather than stall capture.
                        ring.push(block)
                        StreamStats.setLevel(peak)
                        peak = 0f
                        fill = 0
                    }
                }
            }
        } catch (e: Exception) {
            StreamStats.errorText = "Capture failed: ${e.message}"
        } finally {
            runCatching { record.stop() }
            runCatching { record.release() }
        }
    }

    private fun sendLoop() {
        var socket: DatagramSocket? = null
        try {
            socket = DatagramSocket()
            val packet = ByteArray(Protocol.PACKET_LEN)
            val block = ShortArray(Config.BLOCK_SAMPLES)
            val datagram = DatagramPacket(packet, packet.size, destination, destinationPort)
            var seq = 0
            val startedNs = System.nanoTime()
            // Pacing clock: when the next datagram is due to leave. Sending a
            // whole capture buffer as a burst is what forces the far end to
            // run a deep jitter buffer; spreading them costs no latency.
            var nextSendNs = System.nanoTime()
            val blockNs = Config.FRAMES_PER_BLOCK.toLong() * 1_000_000_000L / Config.SAMPLE_RATE

            while (!stop.get()) {
                if (!ring.pop(block)) {
                    Thread.sleep(1)
                    nextSendNs = maxOf(nextSendNs, System.nanoTime())
                    continue
                }

                if (ring.size() <= 3) {
                    val waitNs = nextSendNs - System.nanoTime()
                    if (waitNs > 0) {
                        if (waitNs <= blockNs * 4) {
                            Thread.sleep(waitNs / 1_000_000, (waitNs % 1_000_000).toInt())
                        } else {
                            nextSendNs = System.nanoTime() // clock ran away; resync
                        }
                    }
                }

                val tsUs = ((System.nanoTime() - startedNs) / 1000L).toInt()
                Protocol.encodeAudio(packet, Protocol.MAGIC, token, seq, tsUs, block)
                seq++
                nextSendNs += blockNs
                if (nextSendNs + blockNs * 2 < System.nanoTime()) {
                    nextSendNs = System.nanoTime()
                }

                try {
                    socket.send(datagram)
                    StreamStats.blocks.incrementAndGet()
                    StreamStats.lastPacketMs.set(System.currentTimeMillis())
                } catch (e: Exception) {
                    // Normal while the peer reboots or WiFi renegotiates.
                    StreamStats.errorText = "Send failed: ${e.message}"
                    Thread.sleep(2)
                }
            }
        } catch (e: Exception) {
            StreamStats.errorText = "Could not open a socket: ${e.message}"
        } finally {
            socket?.close()
        }
    }

    companion object {
        fun isSupported(): Boolean = Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q
    }
}
