package com.kosherplay.audiopush

import android.content.Context

/**
 * Remembered choices, kept between runs.
 *
 * Preferences only, so a missing or unreadable value is never an error: every
 * field falls back to its default.
 */
class Settings(context: Context) {
    private val prefs =
        context.applicationContext.getSharedPreferences("audiopush", Context.MODE_PRIVATE)

    /** The PC last streamed to, offered again on the next run. */
    var lastHost: String?
        get() = prefs.getString(KEY_HOST, null)?.takeIf { it.isNotBlank() }
        set(value) = prefs.edit().putString(KEY_HOST, value).apply()

    var lastPort: Int
        get() = prefs.getInt(KEY_PORT, Config.AUDIO_PORT)
        set(value) = prefs.edit().putInt(KEY_PORT, value).apply()

    /** Its hostname at the time, for a friendlier label than a bare IP. */
    var lastName: String?
        get() = prefs.getString(KEY_NAME, null)?.takeIf { it.isNotBlank() }
        set(value) = prefs.edit().putString(KEY_NAME, value).apply()

    var preset: Latency
        get() = Latency.entries.getOrElse(prefs.getInt(KEY_PRESET, Latency.BALANCED.ordinal)) {
            Latency.BALANCED
        }
        set(value) = prefs.edit().putInt(KEY_PRESET, value.ordinal).apply()

    /** Forgets the saved PC, leaving the preset alone. */
    fun forgetTarget() {
        prefs.edit().remove(KEY_HOST).remove(KEY_PORT).remove(KEY_NAME).apply()
    }

    private companion object {
        const val KEY_HOST = "last_host"
        const val KEY_PORT = "last_port"
        const val KEY_NAME = "last_name"
        const val KEY_PRESET = "preset"
    }
}
