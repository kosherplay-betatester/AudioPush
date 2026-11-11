package com.kosherplay.audiopush

import java.security.MessageDigest
import java.security.SecureRandom
import javax.crypto.Mac
import javax.crypto.spec.SecretKeySpec

/**
 * Pairing secrets and packet authentication. Mirrors `pairing.rs` byte for
 * byte, so a phone and a PC pair with each other.
 *
 * The model: a receiver shows a six-digit PIN. The sender proves it knows
 * that PIN and is given a random 256-bit token. Every audio and microphone
 * packet then carries a truncated HMAC-SHA256 tag computed with that token,
 * so an unpaired machine on the network can neither play audio through your
 * speakers nor redirect your microphone to itself.
 *
 * Six digits is only a million combinations, which is nothing to a machine
 * guessing over UDP. What makes it safe is [PairingHost]: a handful of wrong
 * guesses and the PIN is thrown away and a new one issued.
 */
object Crypto {
    /** Length of the tag carried by every audio packet. */
    const val TAG_LEN = 8

    private val random = SecureRandom()

    fun randomBytes(count: Int): ByteArray = ByteArray(count).also { random.nextBytes(it) }

    fun hmac(key: ByteArray, data: ByteArray, length: Int = data.size): ByteArray {
        val mac = Mac.getInstance("HmacSHA256")
        mac.init(SecretKeySpec(key, "HmacSHA256"))
        mac.update(data, 0, length)
        return mac.doFinal()
    }

    fun sha256(data: ByteArray): ByteArray =
        MessageDigest.getInstance("SHA-256").digest(data)

    /** Comparison that does not leak how many bytes matched. */
    fun constantTimeEquals(a: ByteArray, aFrom: Int, b: ByteArray, bFrom: Int, length: Int): Boolean {
        if (a.size < aFrom + length || b.size < bFrom + length) return false
        var diff = 0
        for (i in 0 until length) {
            diff = diff or (a[aFrom + i].toInt() xor b[bFrom + i].toInt())
        }
        return diff == 0
    }
}

/** A pairing secret shared by one sender and one receiver. */
class Token(val bytes: ByteArray) {
    init {
        require(bytes.size == 32) { "a token is 32 bytes" }
    }

    /**
     * Short, non-secret identifier carried in each packet so the receiver can
     * pick the right token in one step instead of trying all of them.
     */
    val keyId: Int by lazy {
        val digest = Crypto.sha256(bytes)
        (digest[0].toInt() and 0xFF) or
            ((digest[1].toInt() and 0xFF) shl 8) or
            ((digest[2].toInt() and 0xFF) shl 16) or
            ((digest[3].toInt() and 0xFF) shl 24)
    }

    fun toHex(): String = bytes.joinToString("") { "%02x".format(it) }

    /** Never print the secret, not even by accident in a log line. */
    override fun toString(): String = "Token(%08x)".format(keyId)

    override fun equals(other: Any?): Boolean =
        other is Token && bytes.contentEquals(other.bytes)

    override fun hashCode(): Int = bytes.contentHashCode()

    companion object {
        fun random(): Token = Token(Crypto.randomBytes(32))

        fun fromHex(text: String): Token? {
            if (text.length != 64) return null
            val bytes = ByteArray(32)
            for (i in 0 until 32) {
                bytes[i] = text.substring(i * 2, i * 2 + 2).toIntOrNull(16)?.toByte() ?: return null
            }
            return Token(bytes)
        }
    }
}

/** A six-digit pairing PIN, held only in memory and only while pairing. */
class Pin(val digits: String) {
    /** Grouped for reading aloud across a room. */
    fun display(): String = "${digits.substring(0, 3)} ${digits.substring(3)}"

    /** Proof that both ends know the PIN, over whatever context they agree on. */
    fun proof(context: ByteArray): ByteArray =
        Crypto.hmac(digits.toByteArray(Charsets.UTF_8), context)

    fun verifyProof(context: ByteArray, proof: ByteArray): Boolean {
        if (proof.size != 32) return false
        return Crypto.constantTimeEquals(this.proof(context), 0, proof, 0, 32)
    }

    companion object {
        fun random(): Pin {
            val bytes = Crypto.randomBytes(4)
            var value = 0L
            for (i in 0 until 4) value = value or ((bytes[i].toLong() and 0xFF) shl (8 * i))
            return Pin("%06d".format(value % 1_000_000L))
        }

        fun parse(text: String): Pin? {
            val digits = text.filter { it.isDigit() }
            return if (digits.length == 6) Pin(digits) else null
        }
    }
}

/**
 * The tokens this device accepts packets signed with. Lookup is by key id, so
 * verifying a packet costs one HMAC rather than one per token.
 */
class TokenStore(initial: List<Token>) {
    private val tokens = ArrayList(initial)

    @Synchronized
    fun find(keyId: Int): Token? = tokens.firstOrNull { it.keyId == keyId }

    /**
     * Adds a token the moment a new sender pairs, so it can start streaming
     * without the receiver being restarted.
     */
    @Synchronized
    fun add(token: Token) {
        if (tokens.none { it == token }) tokens.add(token)
    }
}

/** Receiver side: issues the PIN and hands out tokens. */
class PairingHost {
    private var pin = Pin.random()
    private var attempts = 0
    private var windowUntilMs = 0L
    private var lockedUntilMs = 0L
    private var lockoutMs = FIRST_LOCKOUT_MS
    private var lastAttemptMs = 0L
    private val recentNonces = ArrayDeque<String>()

    /**
     * Opens a pairing window: a fresh PIN, answered for two minutes.
     *
     * Everything about this design rests on the window. Six digits is a
     * million combinations, which a machine guesses in seconds if it is
     * allowed to keep trying; it is safe only because almost no guesses are
     * accepted. Rotating the PIN on failure is *not* a substitute, because an
     * attacker does not care which PIN they hit -- it leaves the expected
     * work at one in a million either way.
     */
    @Synchronized
    fun openWindow() {
        pin = Pin.random()
        attempts = 0
        windowUntilMs = System.currentTimeMillis() + WINDOW_MS
        lockedUntilMs = 0L
        lockoutMs = FIRST_LOCKOUT_MS
        lastAttemptMs = 0L
    }

    @Synchronized
    fun closeWindow() {
        windowUntilMs = 0L
    }

    /** The PIN to show, or null when no window is open. */
    @Synchronized
    fun displayPin(): String? =
        if (System.currentTimeMillis() < windowUntilMs) pin.display() else null

    /** Seconds left in the open window, for the countdown on screen. */
    @Synchronized
    fun windowSecondsLeft(): Long =
        ((windowUntilMs - System.currentTimeMillis()) / 1000L).coerceAtLeast(0L)

    /** Seconds pairing stays refused after too many wrong guesses. */
    @Synchronized
    fun lockedSecondsLeft(): Long =
        ((lockedUntilMs - System.currentTimeMillis()) / 1000L).coerceAtLeast(0L)

    /** Judges one pairing request. Returns the token on success. */
    @Synchronized
    fun handleRequest(nonce: ByteArray, proof: ByteArray): Outcome {
        val now = System.currentTimeMillis()

        // Outside an open window nothing is answered at all.
        if (now >= windowUntilMs) return Outcome.Refused(Protocol.REJECT_NOT_OPEN)

        // A spent window stays locked, for longer each time.
        if (now < lockedUntilMs) return Outcome.Refused(Protocol.REJECT_TOO_MANY)
        if (lockedUntilMs != 0L) {
            lockedUntilMs = 0L
            attempts = 0
        }

        // Attempts cannot be spent in a burst.
        if (lastAttemptMs != 0L && now - lastAttemptMs < MIN_ATTEMPT_GAP_MS) {
            return Outcome.Refused(Protocol.REJECT_TOO_MANY)
        }
        lastAttemptMs = now

        // A replayed request must not consume an attempt or succeed twice.
        val fingerprint = nonce.joinToString("") { "%02x".format(it) }
        if (recentNonces.contains(fingerprint)) {
            return Outcome.Refused(Protocol.REJECT_WRONG_PIN)
        }
        if (recentNonces.size == NONCE_MEMORY) recentNonces.removeFirst()
        recentNonces.addLast(fingerprint)

        if (!pin.verifyProof(nonce, proof)) {
            attempts++
            if (attempts >= MAX_ATTEMPTS) {
                lockedUntilMs = now + lockoutMs
                lockoutMs = (lockoutMs * 2).coerceAtMost(MAX_LOCKOUT_MS)
                return Outcome.Refused(Protocol.REJECT_TOO_MANY)
            }
            return Outcome.Refused(Protocol.REJECT_WRONG_PIN)
        }

        // Success closes the window and burns the PIN, so one PIN pairs one
        // device. Anyone who overheard it has nothing left to use.
        attempts = 0
        windowUntilMs = 0L
        pin = Pin.random()
        return Outcome.Paired(Token.random())
    }

    /**
     * Proof that the receiver knew the PIN too, so a machine impersonating
     * the receiver cannot hand out a token of its own choosing.
     */
    @Synchronized
    fun acceptProof(nonce: ByteArray, token: Token): ByteArray =
        pin.proof(nonce + token.bytes)

    sealed class Outcome {
        data class Paired(val token: Token) : Outcome()
        data class Refused(val reason: Byte) : Outcome()
    }

    private companion object {
        const val MAX_ATTEMPTS = 5
        const val MIN_ATTEMPT_GAP_MS = 500L
        const val WINDOW_MS = 120_000L
        const val FIRST_LOCKOUT_MS = 30_000L
        const val MAX_LOCKOUT_MS = 900_000L
        const val NONCE_MEMORY = 64
    }
}
