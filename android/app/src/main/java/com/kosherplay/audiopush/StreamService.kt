package com.kosherplay.audiopush

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.media.projection.MediaProjection
import android.media.projection.MediaProjectionManager
import android.os.Build
import android.os.IBinder
import java.net.InetAddress

/**
 * Keeps the stream alive when the screen goes off or the app is backgrounded.
 *
 * Android stops ordinary threads from holding the CPU and the radio once the
 * app loses focus, so both directions have to live in a foreground service.
 * It also owns the WiFi locks, which are what stop the radio sleeping between
 * beacons and delivering packets in late bursts.
 */
class StreamService : Service() {

    private var receiver: ReceiverEngine? = null
    private var sender: SenderEngine? = null
    private lateinit var settings: Settings

    /**
     * Issues this device's pairing PIN while it is receiving. A single
     * instance for the process, so the PIN the UI shows is the one the
     * announcer thread checks against.
     */
    private val pairingHost: PairingHost get() = sharedPairingHost
    private var announcer: Announcer? = null
    private var locks: WifiLocks? = null
    private var projection: MediaProjection? = null

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onCreate() {
        super.onCreate()
        settings = Settings(this)
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        when (intent?.action) {
            ACTION_START_RECEIVER -> startReceiver(intent)
            ACTION_START_SENDER -> startSender(intent)
            else -> stopEverything()
        }
        return START_NOT_STICKY
    }

    private fun startReceiver(intent: Intent) {
        stopEverything()
        val preset = presetFrom(intent)

        startForegroundCompat(
            "Playing audio from your PC",
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
                ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PLAYBACK
            } else {
                0
            }
        )

        locks = WifiLocks(this).also { it.acquire() }

        // Only machines this device has paired with may stream to it.
        val tokens = TokenStore(settings.allTokens())
        receiver = ReceiverEngine(this, preset, tokens).also { it.start() }
        // Announce so the PC finds this phone by name instead of by IP.
        announcer = Announcer(Config.AUDIO_PORT, pairingHost, tokens) { address, token ->
            // Persist the pairing so it survives a restart.
            val host = address.hostAddress ?: return@Announcer
            settings.addPairing(host, host, token)
        }.also { it.start() }
    }

    private fun startSender(intent: Intent) {
        stopEverything()
        if (!SenderEngine.isSupported()) {
            StreamStats.errorText = "Capturing this phone's audio needs Android 10 or newer."
            stopSelf()
            return
        }
        val preset = presetFrom(intent)
        val host = intent.getStringExtra(EXTRA_HOST)
        val port = intent.getIntExtra(EXTRA_PORT, Config.AUDIO_PORT)
        val resultCode = intent.getIntExtra(EXTRA_RESULT_CODE, 0)
        val resultData: Intent? = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            intent.getParcelableExtra(EXTRA_RESULT_DATA, Intent::class.java)
        } else {
            @Suppress("DEPRECATION")
            intent.getParcelableExtra(EXTRA_RESULT_DATA)
        }
        if (host == null || resultData == null) {
            stopSelf()
            return
        }

        // Android 14 insists the foreground service is already running before
        // the projection is handed over, so this order is not optional.
        startForegroundCompat(
            "Streaming this phone's audio to $host",
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
                ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PROJECTION
            } else {
                0
            }
        )

        val manager = getSystemService(Context.MEDIA_PROJECTION_SERVICE) as MediaProjectionManager
        val media = try {
            manager.getMediaProjection(resultCode, resultData)
        } catch (e: Exception) {
            StreamStats.errorText = "Screen-capture permission was refused: ${e.message}"
            stopSelf()
            return
        }
        if (media == null) {
            StreamStats.errorText = "Screen-capture permission was refused."
            stopSelf()
            return
        }
        projection = media
        media.registerCallback(object : MediaProjection.Callback() {
            override fun onStop() {
                stopEverything()
                stopSelf()
            }
        }, null)

        // Without a pairing there is nothing to sign packets with, and the
        // PC would reject every one of them.
        val token = settings.tokenFor(host)
        if (token == null) {
            StreamStats.errorText = "Not paired with $host yet."
            runCatching { media.stop() }
            stopSelf()
            return
        }

        locks = WifiLocks(this).also { it.acquire() }
        sender = SenderEngine(media, InetAddress.getByName(host), port, token)
            .also { it.start() }
    }

    private fun presetFrom(intent: Intent): Latency {
        val ordinal = intent.getIntExtra(EXTRA_PRESET, Latency.BALANCED.ordinal)
        return Latency.entries.getOrElse(ordinal) { Latency.BALANCED }
    }

    private fun stopEverything() {
        announcer?.stop()
        receiver?.stop()
        sender?.stop()
        locks?.release()
        runCatching { projection?.stop() }
        announcer = null
        receiver = null
        sender = null
        locks = null
        projection = null
    }

    override fun onDestroy() {
        stopEverything()
        super.onDestroy()
    }

    private fun startForegroundCompat(text: String, type: Int) {
        val manager = getSystemService(NotificationManager::class.java)
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            manager.createNotificationChannel(
                NotificationChannel(
                    CHANNEL_ID,
                    "AudioPush stream",
                    NotificationManager.IMPORTANCE_LOW
                ).apply { setShowBadge(false) }
            )
        }

        val open = PendingIntent.getActivity(
            this,
            0,
            Intent(this, MainActivity::class.java),
            PendingIntent.FLAG_IMMUTABLE
        )
        val stopIntent = PendingIntent.getService(
            this,
            1,
            Intent(this, StreamService::class.java).setAction(ACTION_STOP),
            PendingIntent.FLAG_IMMUTABLE
        )

        val notification: Notification = Notification.Builder(this, CHANNEL_ID)
            .setContentTitle("AudioPush")
            .setContentText(text)
            .setSmallIcon(android.R.drawable.ic_lock_silent_mode_off)
            .setOngoing(true)
            .setContentIntent(open)
            .addAction(
                Notification.Action.Builder(null, "Stop", stopIntent).build()
            )
            .build()

        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q && type != 0) {
            startForeground(NOTIFICATION_ID, notification, type)
        } else {
            startForeground(NOTIFICATION_ID, notification)
        }
    }

    companion object {
        /**
         * Shared between the service and the UI: the activity shows the
         * PIN, the service's announcer thread checks answers against it.
         */
        val sharedPairingHost = PairingHost()

        const val ACTION_START_RECEIVER = "com.kosherplay.audiopush.START_RECEIVER"
        const val ACTION_START_SENDER = "com.kosherplay.audiopush.START_SENDER"
        const val ACTION_STOP = "com.kosherplay.audiopush.STOP"
        const val EXTRA_PRESET = "preset"
        const val EXTRA_HOST = "host"
        const val EXTRA_PORT = "port"
        const val EXTRA_RESULT_CODE = "resultCode"
        const val EXTRA_RESULT_DATA = "resultData"

        private const val CHANNEL_ID = "audiopush"
        private const val NOTIFICATION_ID = 1

        fun stop(context: Context) {
            context.startService(
                Intent(context, StreamService::class.java).setAction(ACTION_STOP)
            )
        }
    }
}
