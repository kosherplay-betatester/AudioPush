package com.kosherplay.audiopush

import android.content.Context
import android.media.AudioFormat
import android.media.AudioRecord
import android.media.MediaRecorder
import android.media.audiofx.AcousticEchoCanceler
import android.media.audiofx.NoiseSuppressor
import android.os.Process
import android.util.Log
import java.net.DatagramPacket
import java.net.DatagramSocket
import java.util.concurrent.atomic.AtomicBoolean

/**
 * Lends this device's microphone to the PC that is streaming audio to it.
 *
 * Runs only while the receiver is running and only while the switch is on:
 * the microphone is opened when this engine starts and released when it
 * stops, so the app is never quietly listening.
 *
 * The destination is not known in advance. The receiver learns who is
 * streaming to it when the first audio packet arrives and writes that address
 * into [target]; until then this engine captures nothing.
 */
class MicEngine(private val context: Context, private val target: PeerTarget) {
    private val stop = AtomicBoolean(false)
    private val ring = BlockRing(Config.RING_BLOCKS)
    private var captureThread: Thread? = null
    private var netThread: Thread? = null

    fun start() {
        stop.set(false)
        MicStats.reset()
        MicStats.running.set(true)
        captureThread = Thread({ captureLoop() }, "audiopush-mic").apply {
            isDaemon = true
            start()
        }
        netThread = Thread({ sendLoop() }, "audiopush-mic-send").apply {
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
        MicStats.running.set(false)
        MicStats.setLevel(0f)
    }

    private fun captureLoop() {
        Process.setThreadPriority(Process.THREAD_PRIORITY_URGENT_AUDIO)

        val minBytes = AudioRecord.getMinBufferSize(
            Config.SAMPLE_RATE,
            AudioFormat.CHANNEL_IN_MONO,
            AudioFormat.ENCODING_PCM_16BIT
        )
        if (minBytes <= 0) {
            MicStats.errorText = "This device rejected 48 kHz microphone capture."
            return
        }

        val record = try {
            AudioRecord(
                // VOICE_COMMUNICATION enables the platform echo canceller and
                // noise suppressor, which is what you want when the far end is
                // playing audio through speakers in the same room.
                MediaRecorder.AudioSource.VOICE_COMMUNICATION,
                Config.SAMPLE_RATE,
                AudioFormat.CHANNEL_IN_MONO,
                AudioFormat.ENCODING_PCM_16BIT,
                minBytes * 2
            )
        } catch (e: SecurityException) {
            MicStats.errorText = "Microphone permission was not granted."
            return
        } catch (e: Exception) {
            MicStats.errorText = "Could not open the microphone: ${e.message}"
            return
        }

        if (record.state != AudioRecord.STATE_INITIALIZED) {
            MicStats.errorText = "Could not open the microphone. Is another app using it?"
            runCatching { record.release() }
            return
        }

        // This phone is playing the PC's audio out of its own speaker while
        // the microphone listens in the same room, so the microphone hears it
        // and sends it back. VOICE_COMMUNICATION asks the platform for echo
        // cancellation, but whether it arrives is up to the device; attaching
        // the effects to the session is the part that can be checked.
        // Neither is available on every phone, hence the guards.
        val effects = mutableListOf<android.media.audiofx.AudioEffect>()
        if (AcousticEchoCanceler.isAvailable()) {
            runCatching {
                AcousticEchoCanceler.create(record.audioSessionId)?.also {
                    it.enabled = true
                    effects.add(it)
                }
            }
        } else {
            Log.w("AudioPush", "no echo canceller on this device; expect feedback on speaker")
        }
        if (NoiseSuppressor.isAvailable()) {
            runCatching {
                NoiseSuppressor.create(record.audioSessionId)?.also {
                    it.enabled = true
                    effects.add(it)
                }
            }
        }

        // Pre-allocated; the loop never asks the allocator for anything.
        val mono = ShortArray(Config.FRAMES_PER_BLOCK * 2)
        val block = ShortArray(Config.BLOCK_SAMPLES)
        var fill = 0
        var peak = 0f

        try {
            record.startRecording()
            MicStats.errorText = null
            Log.i(TAG, "microphone open at ${Config.SAMPLE_RATE} Hz mono")

            while (!stop.get()) {
                val read = record.read(mono, 0, mono.size)
                if (read <= 0) {
                    Thread.sleep(2)
                    continue
                }
                for (i in 0 until read) {
                    val sample = mono[i]
                    val magnitude = kotlin.math.abs(sample.toInt()) / 32768f
                    if (magnitude > peak) peak = magnitude
                    // The wire format is stereo, so a mono microphone is
                    // written to both channels. It costs bandwidth that is
                    // free on a LAN and keeps one format end to end.
                    block[fill++] = sample
                    block[fill++] = sample
                    if (fill == Config.BLOCK_SAMPLES) {
                        ring.push(block)
                        MicStats.setLevel(peak)
                        peak = 0f
                        fill = 0
                    }
                }
            }
        } catch (e: Exception) {
            MicStats.errorText = "Microphone capture failed: ${e.message}"
        } finally {
            runCatching { record.stop() }
            effects.forEach { effect -> runCatching { effect.release() } }
            runCatching { record.release() }
            Log.i(TAG, "microphone released")
        }
    }

    private fun sendLoop() {
        var socket: DatagramSocket? = null
        try {
            socket = DatagramSocket()
            val packet = ByteArray(Protocol.PACKET_LEN)
            val block = ShortArray(Config.BLOCK_SAMPLES)
            var seq = 0
            val startedNs = System.nanoTime()
            var nextSendNs = System.nanoTime()
            val blockNs = Config.FRAMES_PER_BLOCK.toLong() * 1_000_000_000L / Config.SAMPLE_RATE

            while (!stop.get()) {
                if (!ring.pop(block)) {
                    Thread.sleep(1)
                    nextSendNs = maxOf(nextSendNs, System.nanoTime())
                    continue
                }

                val address = target.address
                val signingKey = target.token
                if (address != null && MicStats.targetText == null) {
                    MicStats.targetText = address.hostAddress
                }
                if (address == null || signingKey == null) {
                    // Nobody is streaming to us yet, so there is nowhere to
                    // send. The block is dropped rather than staled.
                    continue
                }

                // Pacing, same reasoning as the main stream: an even trickle
                // is far easier for the far end to buffer than a burst.
                if (ring.size() <= 3) {
                    val waitNs = nextSendNs - System.nanoTime()
                    if (waitNs > 0) {
                        if (waitNs <= blockNs * 4) {
                            Thread.sleep(waitNs / 1_000_000, (waitNs % 1_000_000).toInt())
                        } else {
                            nextSendNs = System.nanoTime()
                        }
                    }
                }

                val tsUs = ((System.nanoTime() - startedNs) / 1000L).toInt()
                Protocol.encodeAudio(packet, Protocol.MIC_MAGIC, signingKey, seq, tsUs, block)
                seq++
                nextSendNs += blockNs
                if (nextSendNs + blockNs * 2 < System.nanoTime()) {
                    nextSendNs = System.nanoTime()
                }

                try {
                    socket.send(DatagramPacket(packet, packet.size, address, Config.MIC_PORT))
                    MicStats.blocks.incrementAndGet()
                    MicStats.lastPacketMs.set(System.currentTimeMillis())
                } catch (e: Exception) {
                    MicStats.errorText = "Microphone send failed: ${e.message}"
                    Thread.sleep(2)
                }
            }
        } catch (e: Exception) {
            MicStats.errorText = "Could not open a socket for the microphone: ${e.message}"
        } finally {
            socket?.close()
        }
    }

    private companion object {
        const val TAG = "AudioPush"
    }
}
