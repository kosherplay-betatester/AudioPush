package com.kosherplay.audiopush

import android.content.Context
import android.media.AudioAttributes
import android.media.AudioDeviceCallback
import android.media.AudioDeviceInfo
import android.media.AudioFormat
import android.media.AudioManager
import android.media.AudioTrack
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.os.Process
import android.util.Log
import java.net.DatagramPacket
import java.net.DatagramSocket
import java.net.InetSocketAddress
import java.util.concurrent.atomic.AtomicBoolean

/**
 * Phone as speaker: UDP in, jitter buffer, AudioTrack out.
 *
 * Mirrors the Windows receiver, including the adaptive buffer depth, the
 * one-frame clock drift correction and the fade-out concealment. The only
 * structural difference is that `AudioTrack.write` blocks, so it paces the
 * loop in place of a buffer-ready event.
 */
class ReceiverEngine(
    private val context: Context,
    private val preset: Latency,
    /** Every pairing this device will accept audio from. */
    private val tokens: TokenStore,
    /** Filled in so the return microphone stream knows where to send. */
    private val micTarget: PeerTarget? = null
) {
    private val stop = AtomicBoolean(false)

    /** Set when the output devices change, so the track is reopened. */
    private val rebuild = AtomicBoolean(false)

    private val ring = BlockRing(Config.RING_BLOCKS)
    private var netThread: Thread? = null
    private var playThread: Thread? = null

    /** Consecutive writes that took nothing; see [writeChecked]. */
    private var zeroWrites = 0

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
            var unauthorised = 0L

            while (!stop.get()) {
                val packet = DatagramPacket(buffer, buffer.size)
                try {
                    socket.receive(packet)
                } catch (_: Exception) {
                    continue // timeout; re-check the stop flag
                }

                val header = Protocol.decodeAudioHeader(
                    packet.data, packet.length, Protocol.MAGIC
                ) ?: continue
                val (seq, keyId) = header

                // Authenticate before anything else looks at the packet.
                // Without this an unpaired machine on the network could
                // play audio through these speakers and, far worse,
                // become the destination of this phone's microphone.
                val token = tokens.find(keyId)
                if (token == null || !Protocol.verifyAudio(
                        packet.data, packet.length, token
                    )
                ) {
                    unauthorised++
                    if (unauthorised == 1L || unauthorised % 500L == 0L) {
                        StreamStats.errorText =
                            "Ignoring audio from an unpaired PC " +
                            "(${packet.address.hostAddress}). Pair it to allow it."
                    }
                    continue
                }

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
                    // Point the return microphone stream at whoever started
                    // streaming to us -- once, and only once.
                    //
                    // Following the peer here would be an eavesdropping hole:
                    // anyone on the network could send a single packet to the
                    // audio port and this phone would redirect its live
                    // microphone to them, silently. The destination is latched
                    // for the session instead, and shown in the UI, so a later
                    // arrival cannot steal the microphone. Stopping and
                    // starting the stream is what re-pairs it.
                    micTarget?.let { target ->
                        if (target.address == null) {
                            target.address = packet.address
                            // Sign the microphone with the same pairing
                            // this sender authenticated with.
                            target.token = token
                        }
                    }
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

    /**
     * Owns the AudioTrack across output changes.
     *
     * Android does not keep a low-latency track alive when the device it was
     * routed to disappears. Disconnect Bluetooth and the track is invalidated:
     * `write` starts returning `ERROR_DEAD_OBJECT` and audio stops, even
     * though packets keep arriving and every counter keeps moving. The fix is
     * to notice and open a new track on whatever the current output is, which
     * is what this outer loop does.
     */
    private fun playLoop() {
        Process.setThreadPriority(Process.THREAD_PRIORITY_URGENT_AUDIO)

        val audioManager = context.getSystemService(Context.AUDIO_SERVICE) as AudioManager
        val deviceCallback = object : AudioDeviceCallback() {
            override fun onAudioDevicesAdded(added: Array<out AudioDeviceInfo>?) {
                rebuild.set(true)
            }

            override fun onAudioDevicesRemoved(removed: Array<out AudioDeviceInfo>?) {
                rebuild.set(true)
            }
        }
        runCatching {
            audioManager.registerAudioDeviceCallback(
                deviceCallback,
                Handler(Looper.getMainLooper())
            )
        }

        var consecutiveFailures = 0
        try {
            while (!stop.get()) {
                rebuild.set(false)
                if (runOneTrack()) {
                    consecutiveFailures = 0
                } else {
                    consecutiveFailures++
                    if (consecutiveFailures >= 20) {
                        StreamStats.errorText =
                            "Could not open any audio output. Is another app holding it?"
                        break
                    }
                }
                if (!stop.get()) {
                    // Let the platform settle the new route before reopening.
                    Thread.sleep(150)
                }
            }
        } catch (_: InterruptedException) {
            // Shutting down.
        } finally {
            runCatching { audioManager.unregisterAudioDeviceCallback(deviceCallback) }
        }
    }

    /**
     * Plays until the track is invalidated, the output changes, or we stop.
     * Returns false only if the track could not be opened at all.
     */
    private fun runOneTrack(): Boolean {
        val minBytes = AudioTrack.getMinBufferSize(
            Config.SAMPLE_RATE,
            AudioFormat.CHANNEL_OUT_STEREO,
            AudioFormat.ENCODING_PCM_16BIT
        )
        if (minBytes <= 0) {
            StreamStats.errorText = "This device rejected 48 kHz stereo playback."
            return false
        }
        val deviceBufferFrames = minBytes / (Config.CHANNELS * 2)
        val track = buildTrack(minBytes) ?: return false
        zeroWrites = 0

        // Recomputed per track, because a Bluetooth output and the built-in
        // speaker have very different buffer sizes.
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
            forceRealOutput(track)
            track.play()
            StreamStats.errorText = null
            reportRouting(track, minBytes, deviceBufferFrames)
            // Whatever queued up while the output was away is stale audio.
            ring.trimTo(target)

            while (!stop.get() && !rebuild.get()) {
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
                        java.util.Arrays.fill(conceal, 0)
                        if (!writeChecked(track, conceal, 0, Config.BLOCK_SAMPLES)) return true
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
                    if (!writeChecked(track, conceal, 0, Config.BLOCK_SAMPLES)) return true
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
                // frame more (20 us) holds the buffer centred indefinitely and
                // is inaudible.
                val ok = when {
                    depth > target + 1 ->
                        writeChecked(track, block, 0, Config.BLOCK_SAMPLES - 2)

                    depth + 1 < target ->
                        writeChecked(track, block, 0, Config.BLOCK_SAMPLES) &&
                            writeChecked(track, block, Config.BLOCK_SAMPLES - 2, 2)

                    else -> writeChecked(track, block, 0, Config.BLOCK_SAMPLES)
                }
                if (!ok) return true
            }
        } catch (e: Exception) {
            StreamStats.errorText = "Playback failed: ${e.message}"
        } finally {
            runCatching { track.stop() }
            runCatching { track.release() }
        }
        return true
    }

    /**
     * Writes, and checks the result. Every write in this class goes through
     * here, because the failure modes are silent in two different ways.
     *
     * A negative return is the platform saying the track is gone -- usually
     * its output device went away.
     *
     * A return of **zero** is the subtler one. `write` is blocking, so it
     * should only come back once the track has taken the samples; returning
     * nothing, repeatedly and instantly, means the track is not consuming at
     * all. Treating that as success turns this loop into a spin that drains
     * the jitter buffer as fast as the network fills it: no audio, a level
     * meter flickering at an impossible rate, and a busy core. That is the
     * state a Bluetooth disconnect leaves the track in.
     */
    private fun writeChecked(
        track: AudioTrack,
        data: ShortArray,
        offset: Int,
        count: Int
    ): Boolean {
        val written = track.write(data, offset, count)
        if (written < 0) {
            // ERROR_DEAD_OBJECT, ERROR_INVALID_OPERATION and friends.
            Log.w(TAG, "write failed with $written; reopening the output")
            rebuild.set(true)
            return false
        }
        if (written == 0) {
            zeroWrites++
            if (zeroWrites > 8) {
                Log.w(TAG, "track stopped accepting audio; reopening the output")
                rebuild.set(true)
                return false
            }
            // Do not spin while deciding.
            Thread.sleep(2)
            return true
        }
        zeroWrites = 0
        return true
    }

    /**
     * Pins playback to a real output device.
     *
     * Left to itself, the platform routed this track to `REMOTE_SUBMIX` - a
     * virtual sink used for screen recording, casting and playback capture.
     * Audio written to it is perfectly healthy and completely inaudible: no
     * error, a moving level meter, and silence from the speaker. Whatever
     * holds that submix is outside this app, so rather than fight it, the
     * track is pinned to the best real output: Bluetooth or a wired headset
     * if one is attached, the built-in speaker otherwise.
     */
    private fun forceRealOutput(track: AudioTrack) {
        val audioManager = context.getSystemService(Context.AUDIO_SERVICE) as AudioManager
        val outputs = runCatching {
            audioManager.getDevices(AudioManager.GET_DEVICES_OUTPUTS).toList()
        }.getOrDefault(emptyList())

        Log.i(TAG, "outputs: " + outputs.joinToString { "${it.type}:${it.productName}" })

        // Preference order matches what a user expects from the phone itself.
        val ranked = listOf(
            AudioDeviceInfo.TYPE_BLUETOOTH_A2DP,
            AudioDeviceInfo.TYPE_USB_HEADSET,
            AudioDeviceInfo.TYPE_USB_DEVICE,
            AudioDeviceInfo.TYPE_WIRED_HEADPHONES,
            AudioDeviceInfo.TYPE_WIRED_HEADSET,
            AudioDeviceInfo.TYPE_BUILTIN_SPEAKER
        )
        val chosen = ranked.firstNotNullOfOrNull { wanted ->
            outputs.firstOrNull { it.type == wanted }
        }
        if (chosen == null) {
            Log.w(TAG, "no real output device found; leaving routing to the platform")
            return
        }
        val ok = runCatching { track.setPreferredDevice(chosen) }.getOrDefault(false)
        Log.i(TAG, "pinned output to type ${chosen.type} (${chosen.productName}): $ok")
    }

    /**
     * Records where playback actually went.
     *
     * "The meter moves but I hear nothing" has several possible causes that
     * look identical from outside: a dead track, a track routed to a device
     * that is no longer there, or a healthy track on an output whose volume
     * is zero. Naming the routed device separates them, so it is shown in the
     * UI and logged.
     */
    private fun reportRouting(track: AudioTrack, bufferBytes: Int, bufferFrames: Int) {
        val routed = runCatching { track.routedDevice }.getOrNull()
        val label = when (routed?.type) {
            null -> "unknown"
            AudioDeviceInfo.TYPE_BUILTIN_SPEAKER -> "Phone speaker"
            AudioDeviceInfo.TYPE_BLUETOOTH_A2DP -> "Bluetooth"
            AudioDeviceInfo.TYPE_BLUETOOTH_SCO -> "Bluetooth (call audio)"
            AudioDeviceInfo.TYPE_WIRED_HEADPHONES -> "Wired headphones"
            AudioDeviceInfo.TYPE_WIRED_HEADSET -> "Wired headset"
            AudioDeviceInfo.TYPE_USB_HEADSET -> "USB headset"
            AudioDeviceInfo.TYPE_USB_DEVICE -> "USB audio"
            else -> "type ${routed.type}"
        }
        val name = routed?.productName?.toString().orEmpty()
        val full = if (name.isBlank() || label.startsWith("type")) label else "$label ($name)"

        val audioManager = context.getSystemService(Context.AUDIO_SERVICE) as AudioManager
        val volume = runCatching {
            val current = audioManager.getStreamVolume(AudioManager.STREAM_MUSIC)
            val max = audioManager.getStreamMaxVolume(AudioManager.STREAM_MUSIC).coerceAtLeast(1)
            current * 100 / max
        }.getOrDefault(-1)

        StreamStats.outputText = if (volume >= 0) "$full · media volume $volume%" else full
        Log.i(
            TAG,
            "track open: routed=$full bufferBytes=$bufferBytes frames=$bufferFrames " +
                "perf=${runCatching { track.performanceMode }.getOrDefault(-1)} " +
                "mediaVolume=$volume%"
        )
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

    private companion object {
        const val TAG = "AudioPush"
    }
}
