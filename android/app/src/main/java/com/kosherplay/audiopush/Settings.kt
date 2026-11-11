package com.kosherplay.audiopush

import android.content.Context

/** A PC streamed to before. */
data class Recent(val host: String, val port: Int, val name: String) {
    fun label(): String = if (name.isBlank()) host else "$name  —  $host"

    fun serialise(): String = "${name.replace('|', ' ')}|$host|$port"

    companion object {
        const val LIMIT = 5

        fun parse(text: String): Recent? {
            val parts = text.split('|')
            if (parts.size != 3) return null
            val port = parts[2].toIntOrNull() ?: return null
            if (parts[1].isBlank()) return null
            return Recent(parts[1], port, parts[0])
        }
    }
}

/**
 * Remembered choices, kept between runs.
 *
 * Preferences only, so a missing or unreadable value is never an error: every
 * field falls back to its default.
 */
class Settings(context: Context) {
    private val prefs =
        context.applicationContext.getSharedPreferences("audiopush", Context.MODE_PRIVATE)

    /** PCs streamed to before, most recent first, at most [Recent.LIMIT]. */
    var recent: List<Recent>
        get() = prefs.getString(KEY_RECENT, null)
            ?.lineSequence()
            ?.mapNotNull { Recent.parse(it) }
            ?.take(Recent.LIMIT)
            ?.toList()
            ?: emptyList()
        set(value) = prefs.edit()
            .putString(KEY_RECENT, value.take(Recent.LIMIT).joinToString("\n") { it.serialise() })
            .apply()

    /**
     * Moves a PC to the front of the list, trimming to the limit.
     *
     * Matching is by host, so reconnecting to a machine that has been renamed
     * updates the name rather than adding a second entry.
     */
    fun pushRecent(entry: Recent) {
        recent = listOf(entry) + recent.filterNot { it.host == entry.host }
    }

    /** The PC to offer on start-up. */
    fun mostRecent(): Recent? = recent.firstOrNull()

    var preset: Latency
        get() = Latency.entries.getOrElse(prefs.getInt(KEY_PRESET, Latency.BALANCED.ordinal)) {
            Latency.BALANCED
        }
        set(value) = prefs.edit().putInt(KEY_PRESET, value.ordinal).apply()

    /** Whether to lend this device's microphone while receiving. */
    var micEnabled: Boolean
        get() = prefs.getBoolean(KEY_MIC, false)
        set(value) = prefs.edit().putBoolean(KEY_MIC, value).apply()

    /**
     * Paired machines, one per line as "host|name|token-hex". The same list
     * serves both roles: a sender looks up the token for the PC it is
     * calling, a receiver trusts every token it has ever issued.
     */
    var pairings: List<Triple<String, String, Token>>
        get() = prefs.getString(KEY_PAIRINGS, null)
            ?.lineSequence()
            ?.mapNotNull { line ->
                val parts = line.split('|')
                if (parts.size != 3) return@mapNotNull null
                val token = Token.fromHex(parts[2]) ?: return@mapNotNull null
                Triple(parts[0], parts[1], token)
            }
            ?.toList()
            ?: emptyList()
        set(value) {
            val text = value.joinToString("\n") {
                it.first + "|" + it.second.replace('|', ' ') + "|" + it.third.toHex()
            }
            prefs.edit().putString(KEY_PAIRINGS, text).apply()
        }

    /** The secret shared with one machine, if any. */
    fun tokenFor(host: String): Token? = pairings.firstOrNull { it.first == host }?.third

    /** Every token this device will accept, for the receiver side. */
    fun allTokens(): List<Token> = pairings.map { it.third }

    /** Records a pairing, replacing any earlier one with the same machine. */
    fun addPairing(host: String, name: String, token: Token) {
        pairings = pairings.filterNot { it.first == host } + Triple(host, name, token)
    }

    /** Revokes every pairing. Both ends must pair again before audio flows. */
    fun clearPairings() {
        prefs.edit().remove(KEY_PAIRINGS).apply()
    }

    /** Forgets every saved PC, leaving the preset and the switch alone. */
    fun forgetTarget() {
        prefs.edit().remove(KEY_RECENT).apply()
    }

    private companion object {
        const val KEY_RECENT = "recent"
        const val KEY_PRESET = "preset"
        const val KEY_MIC = "mic_enabled"
        const val KEY_PAIRINGS = "pairings"
    }
}
