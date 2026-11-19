package com.kosherplay.audiopush

import java.nio.ByteBuffer
import java.nio.ByteOrder

/**
 * The AudioPush wire format, identical to the Windows build.
 *
 * ```
 * offset  size  field
 *      0     4  magic   "APU1"
 *      4     4  seq     u32 LE, increments once per block, wraps
 *      8     4  ts      u32 LE, microseconds since the stream started
 *     12     4  key_id  u32 LE, which pairing token signed this
 *     16   960  payload, 480 int16 LE samples (240 stereo frames)
 *    976     8  tag     HMAC-SHA256(token, bytes 0..976), truncated
 * ```
 *
 * The tag is what stops an unpaired machine on the network playing audio
 * through your speakers.
 */
object Protocol {
    /** Magic for the main stream, sender to receiver. */
    val MAGIC = byteArrayOf('A'.code.toByte(), 'P'.code.toByte(), 'U'.code.toByte(), '1'.code.toByte())


    const val HEADER_LEN = 16

    /** Everything the tag covers: header plus payload. */
    const val SIGNED_LEN = HEADER_LEN + Config.BLOCK_BYTES
    const val PACKET_LEN = SIGNED_LEN + Crypto.TAG_LEN

    /** Writes and signs one audio packet in a reused buffer. No allocation
     *  beyond what the platform MAC needs. */
    fun encodeAudio(
        out: ByteArray,
        magic: ByteArray,
        token: Token,
        seq: Int,
        tsUs: Int,
        block: ShortArray
    ) {
        System.arraycopy(magic, 0, out, 0, 4)
        writeIntLe(out, 4, seq)
        writeIntLe(out, 8, tsUs)
        writeIntLe(out, 12, token.keyId)
        var offset = HEADER_LEN
        for (sample in block) {
            out[offset] = (sample.toInt() and 0xFF).toByte()
            out[offset + 1] = ((sample.toInt() shr 8) and 0xFF).toByte()
            offset += 2
        }
        val tag = Crypto.hmac(token.bytes, out, SIGNED_LEN)
        System.arraycopy(tag, 0, out, SIGNED_LEN, Crypto.TAG_LEN)
    }

    /**
     * Checks shape and magic only, returning the sequence number and key id.
     * The caller then looks up that token and calls [verifyAudio]; nothing is
     * trusted until it does.
     */
    fun decodeAudioHeader(data: ByteArray, length: Int, magic: ByteArray): Pair<Int, Int>? {
        if (length < PACKET_LEN) return null
        for (i in 0..3) if (data[i] != magic[i]) return null
        return Pair(readIntLe(data, 4), readIntLe(data, 12))
    }

    /** True when this packet really was signed by [token]. */
    fun verifyAudio(data: ByteArray, length: Int, token: Token): Boolean {
        if (length < PACKET_LEN) return false
        val expected = Crypto.hmac(token.bytes, data, SIGNED_LEN)
        return Crypto.constantTimeEquals(expected, 0, data, SIGNED_LEN, Crypto.TAG_LEN)
    }

    /** Copies a verified payload into a block of samples. No allocation. */
    fun payloadIntoBlock(data: ByteArray, out: ShortArray) {
        var offset = HEADER_LEN
        for (i in out.indices) {
            val low = data[offset].toInt() and 0xFF
            val high = data[offset + 1].toInt()
            out[i] = ((high shl 8) or low).toShort()
            offset += 2
        }
    }

    // -----------------------------------------------------------------------
    // Discovery and pairing
    // -----------------------------------------------------------------------

    val DISCOVERY_MAGIC =
        byteArrayOf('A'.code.toByte(), 'P'.code.toByte(), 'D'.code.toByte(), '1'.code.toByte())
    const val MSG_ANNOUNCE: Byte = 1
    const val MSG_PROBE: Byte = 2
    const val MSG_PAIR_REQUEST: Byte = 3
    const val MSG_PAIR_ACCEPT: Byte = 4
    const val MSG_PAIR_REJECT: Byte = 5
    const val DISCOVERY_MAX = 160

    const val REJECT_WRONG_PIN: Byte = 1
    const val REJECT_TOO_MANY: Byte = 2

    /** The receiver is not in pairing mode, so nothing is being answered. */
    const val REJECT_NOT_OPEN: Byte = 3

    fun rejectMessage(reason: Byte): String = when (reason) {
        REJECT_TOO_MANY ->
            "Too many wrong attempts. The receiver has stopped answering for a while; " +
                "open pairing on it again."
        REJECT_NOT_OPEN ->
            "The receiver is not in pairing mode. Press \"Pair a new device\" on it first."
        else -> "Wrong PIN. Check the digits shown on the receiver."
    }

    /** "I am <hostname>, send audio to <port>". */
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

    /**
     * Sender to receiver: "here is my nonce and proof that I know your PIN".
     * The PIN itself never crosses the wire.
     */
    fun encodePairRequest(nonce: ByteArray, proof: ByteArray): ByteArray =
        DISCOVERY_MAGIC + MSG_PAIR_REQUEST + nonce + proof

    /** Receiver to sender: "correct, here is the token to sign packets with". */
    fun encodePairAccept(token: Token, hostname: String, proof: ByteArray): ByteArray {
        val nameBytes = hostname.toByteArray(Charsets.UTF_8).let {
            if (it.size > 64) it.copyOf(64) else it
        }
        return DISCOVERY_MAGIC + MSG_PAIR_ACCEPT + token.bytes + proof +
            byteArrayOf(nameBytes.size.toByte()) + nameBytes
    }

    fun encodePairReject(reason: Byte): ByteArray =
        DISCOVERY_MAGIC + MSG_PAIR_REJECT + reason

    sealed class Discovery {
        data class Announce(val hostname: String, val audioPort: Int) : Discovery()
        object Probe : Discovery()
        data class PairRequest(val nonce: ByteArray, val proof: ByteArray) : Discovery() {
            override fun equals(other: Any?): Boolean =
                other is PairRequest && nonce.contentEquals(other.nonce) &&
                    proof.contentEquals(other.proof)

            override fun hashCode(): Int = nonce.contentHashCode() * 31 + proof.contentHashCode()
        }

        data class PairAccept(val token: Token, val proof: ByteArray, val hostname: String) :
            Discovery() {
            override fun equals(other: Any?): Boolean =
                other is PairAccept && token == other.token &&
                    proof.contentEquals(other.proof) && hostname == other.hostname

            override fun hashCode(): Int = token.hashCode() * 31 + hostname.hashCode()
        }

        data class PairReject(val reason: Byte) : Discovery()
    }

    /** Parses a discovery datagram, ignoring anything malformed. */
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
            MSG_PAIR_REQUEST -> {
                if (length < 5 + 16 + 32) return null
                Discovery.PairRequest(
                    data.copyOfRange(5, 21),
                    data.copyOfRange(21, 53)
                )
            }
            MSG_PAIR_ACCEPT -> {
                if (length < 5 + 32 + 32 + 1) return null
                val token = Token(data.copyOfRange(5, 37))
                val proof = data.copyOfRange(37, 69)
                val nameLen = data[69].toInt() and 0xFF
                if (length < 70 + nameLen) return null
                Discovery.PairAccept(token, proof, String(data, 70, nameLen, Charsets.UTF_8))
            }
            MSG_PAIR_REJECT -> {
                if (length < 6) return null
                Discovery.PairReject(data[5])
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
