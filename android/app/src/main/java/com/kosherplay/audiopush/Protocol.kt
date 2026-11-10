package com.kosherplay.audiopush

import java.nio.ByteBuffer
import java.nio.ByteOrder

/**
 * The AudioPush wire format, identical to the Windows build.
 *
 * Audio packet:
 * ```
 * offset  size  field
 *      0     4  magic  "APU1"
 *      4     4  seq    u32 LE
 *      8     4  ts     u32 LE, microseconds since the stream started
 *     12   960  payload, 480 int16 LE samples (240 stereo frames)
 * ```
 */
object Protocol {
    val MAGIC = byteArrayOf('A'.code.toByte(), 'P'.code.toByte(), 'U'.code.toByte(), '1'.code.toByte())
    const val HEADER_LEN = 12
    const val PACKET_LEN = HEADER_LEN + Config.BLOCK_BYTES

    val DISCOVERY_MAGIC =
        byteArrayOf('A'.code.toByte(), 'P'.code.toByte(), 'D'.code.toByte(), '1'.code.toByte())
    const val MSG_ANNOUNCE: Byte = 1
    const val MSG_PROBE: Byte = 2
    const val DISCOVERY_MAX = 4 + 1 + 2 + 1 + 64

    /** Writes one audio packet into a reused buffer. No allocation. */
    fun encodeAudio(out: ByteArray, seq: Int, tsUs: Int, block: ShortArray) {
        System.arraycopy(MAGIC, 0, out, 0, 4)
        writeIntLe(out, 4, seq)
        writeIntLe(out, 8, tsUs)
        var offset = HEADER_LEN
        for (sample in block) {
            out[offset] = (sample.toInt() and 0xFF).toByte()
            out[offset + 1] = ((sample.toInt() shr 8) and 0xFF).toByte()
            offset += 2
        }
    }

    /** Returns the sequence number, or null if this is not an audio packet. */
    fun decodeAudioSeq(data: ByteArray, length: Int): Int? {
        if (length < PACKET_LEN) return null
        for (i in 0..3) if (data[i] != MAGIC[i]) return null
        return readIntLe(data, 4)
    }

    /** Copies a validated payload into a block of samples. No allocation. */
    fun payloadIntoBlock(data: ByteArray, out: ShortArray) {
        var offset = HEADER_LEN
        for (i in out.indices) {
            val low = data[offset].toInt() and 0xFF
            val high = data[offset + 1].toInt()
            out[i] = ((high shl 8) or low).toShort()
            offset += 2
        }
    }

    /** "I am <name>, send audio to <port>". */
    fun encodeAnnounce(name: String, audioPort: Int): ByteArray {
        val nameBytes = name.toByteArray(Charsets.UTF_8).let {
            if (it.size > 64) it.copyOf(64) else it
        }
        val buffer = ByteBuffer.allocate(8 + nameBytes.size).order(ByteOrder.LITTLE_ENDIAN)
        buffer.put(DISCOVERY_MAGIC)
        buffer.put(MSG_ANNOUNCE)
        buffer.putShort(audioPort.toShort())
        buffer.put(nameBytes.size.toByte())
        buffer.put(nameBytes)
        return buffer.array()
    }

    fun encodeProbe(): ByteArray = DISCOVERY_MAGIC + MSG_PROBE

    sealed class Discovery {
        data class Announce(val hostname: String, val audioPort: Int) : Discovery()
        object Probe : Discovery()
    }

    /** Parses a discovery datagram, returning null for anything malformed. */
    fun decodeDiscovery(data: ByteArray, length: Int): Discovery? {
        if (length < 5) return null
        for (i in 0..3) if (data[i] != DISCOVERY_MAGIC[i]) return null
        return when (data[4]) {
            MSG_PROBE -> Discovery.Probe
            MSG_ANNOUNCE -> {
                if (length < 8) return null
                val port = (data[5].toInt() and 0xFF) or ((data[6].toInt() and 0xFF) shl 8)
                val nameLen = data[7].toInt() and 0xFF
                if (length < 8 + nameLen) return null
                Discovery.Announce(String(data, 8, nameLen, Charsets.UTF_8), port)
            }
            else -> null
        }
    }

    private fun writeIntLe(out: ByteArray, offset: Int, value: Int) {
        out[offset] = (value and 0xFF).toByte()
        out[offset + 1] = ((value shr 8) and 0xFF).toByte()
        out[offset + 2] = ((value shr 16) and 0xFF).toByte()
        out[offset + 3] = ((value shr 24) and 0xFF).toByte()
    }

    private fun readIntLe(data: ByteArray, offset: Int): Int =
        (data[offset].toInt() and 0xFF) or
            ((data[offset + 1].toInt() and 0xFF) shl 8) or
            ((data[offset + 2].toInt() and 0xFF) shl 16) or
            ((data[offset + 3].toInt() and 0xFF) shl 24)
}
