package com.kosherplay.audiopush

import android.util.Log
import java.net.HttpURLConnection
import java.net.URL

/**
 * "Is there a newer version?", asked only when the user presses the button.
 *
 * This is the one thing in AudioPush that leaves your network, so it is
 * deliberately the smallest request that answers the question: one GET to the
 * public releases endpoint, carrying nothing about you or this phone. No
 * identifier, no version ping, no telemetry, and nothing happens unless you
 * ask.
 */
object Updates {
    private const val LATEST =
        "https://api.github.com/repos/kosherplay-betatester/AudioPush/releases/latest"

    /** GitHub rejects requests without one. Says nothing about this device. */
    private const val AGENT = "AudioPush-update-check"

    private const val TIMEOUT_MS = 10_000

    private const val RELEASES =
        "https://github.com/kosherplay-betatester/AudioPush/releases"

    data class Release(val version: String, val url: String)

    /** Blocking. Call it off the main thread. */
    fun latest(): Result<Release> = runCatching {
        val connection = (URL(LATEST).openConnection() as HttpURLConnection).apply {
            requestMethod = "GET"
            connectTimeout = TIMEOUT_MS
            readTimeout = TIMEOUT_MS
            setRequestProperty("User-Agent", AGENT)
            setRequestProperty("Accept", "application/vnd.github+json")
        }
        try {
            if (connection.responseCode != 200) {
                error("GitHub answered ${connection.responseCode}")
            }
            val body = connection.inputStream.bufferedReader().use { it.readText() }
            val tag = field(body, "tag_name") ?: error("no release in the reply")
            Release(tag.removePrefix("v"), field(body, "html_url") ?: RELEASES)
        } finally {
            connection.disconnect()
        }
    }.onFailure { Log.w("AudioPush", "update check failed: ${it.message}") }

    /**
     * Pulls one string field out of the JSON.
     *
     * A JSON parser for two fields would be a dependency to keep current for
     * no gain. This reads exactly what it needs and gives up otherwise, which
     * is the right failure: the button says it could not tell.
     */
    private fun field(body: String, name: String): String? {
        val key = "\"$name\":"
        val at = body.indexOf(key)
        if (at < 0) return null
        val rest = body.substring(at + key.length).trimStart()
        if (!rest.startsWith("\"")) return null
        val end = rest.indexOf('"', 1)
        if (end < 0) return null
        return rest.substring(1, end)
    }

    /**
     * True when [other] is a later version than [ours].
     *
     * Compares the numbers rather than the text, so 0.1.10 is correctly newer
     * than 0.1.9 -- which a string comparison gets backwards.
     */
    fun isNewer(other: String, ours: String): Boolean {
        fun parts(v: String) = v.split('.', '-', '+').map { it.toIntOrNull() ?: 0 }
        val a = parts(other)
        val b = parts(ours)
        for (i in 0 until maxOf(a.size, b.size)) {
            val x = a.getOrElse(i) { 0 }
            val y = b.getOrElse(i) { 0 }
            if (x != y) return x > y
        }
        return false
    }
}
