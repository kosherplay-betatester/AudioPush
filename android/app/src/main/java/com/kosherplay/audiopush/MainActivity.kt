package com.kosherplay.audiopush

import android.Manifest
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.media.projection.MediaProjectionManager
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.view.View
import android.widget.ArrayAdapter
import android.widget.CheckBox
import android.widget.Button
import android.widget.EditText
import android.widget.LinearLayout
import android.widget.ListView
import android.widget.ProgressBar
import android.widget.RadioGroup
import android.widget.TextView
import androidx.activity.result.contract.ActivityResultContracts
import androidx.appcompat.app.AppCompatActivity
import androidx.core.content.ContextCompat
import androidx.core.view.ViewCompat
import androidx.core.view.WindowInsetsCompat
import java.net.InetAddress

/**
 * Single screen with two modes, mirroring the Windows app.
 *
 * All streaming lives in [StreamService]; this activity only starts it,
 * stops it, and polls [StreamStats] for the meter and counters.
 */
class MainActivity : AppCompatActivity() {

    private enum class Mode { RECEIVE, SEND }

    private var mode = Mode.RECEIVE
    private var preset = Latency.BALANCED
    private val discovery = DiscoveryListener()
    private var selectedPeer: Peer? = null
    private var selectedRecent: Recent? = null
    /// The list currently shown: live peers first, then remembered ones.
    private var shownTargets: List<Pair<String, Int>> = emptyList()
    private lateinit var settings: Settings

    private val ui = Handler(Looper.getMainLooper())
    private lateinit var tick: Runnable

    private lateinit var modeGroup: RadioGroup
    private lateinit var presetGroup: RadioGroup
    private lateinit var presetHint: TextView
    private lateinit var receiveBox: LinearLayout
    private lateinit var sendBox: LinearLayout
    private lateinit var addressList: TextView
    private lateinit var peerList: ListView
    private lateinit var manualIp: EditText
    private lateinit var batteryBox: View
    private lateinit var batteryButton: Button
    private lateinit var pinText: TextView
    private lateinit var pinHint: TextView
    private lateinit var newPinButton: Button
    private lateinit var revokeButton: Button
    private lateinit var pairBox: View
    private lateinit var pinInput: EditText
    private lateinit var pairButton: Button
    private lateinit var micSwitch: CheckBox
    private lateinit var micMeterRow: View
    private lateinit var micLabel: TextView
    private lateinit var micMeter: ProgressBar
    private lateinit var scanButton: Button
    private lateinit var clearButton: Button
    private lateinit var startButton: Button
    private lateinit var statusText: TextView
    private lateinit var outputText: TextView
    private lateinit var statsText: TextView
    private lateinit var errorText: TextView
    private lateinit var estimateText: TextView
    private lateinit var statusDot: View
    private lateinit var meter: ProgressBar

    private val peerAdapter by lazy {
        ArrayAdapter<String>(this, android.R.layout.simple_list_item_single_choice, ArrayList())
    }

    /** Result of the screen-capture consent dialog, needed to capture audio. */
    private val projectionLauncher =
        registerForActivityResult(ActivityResultContracts.StartActivityForResult()) { result ->
            val data = result.data
            if (result.resultCode != RESULT_OK || data == null) {
                StreamStats.errorText = "Capture permission was declined."
                return@registerForActivityResult
            }
            val host = targetHost()
            if (host == null) {
                StreamStats.errorText = "Choose a PC to send to first."
                return@registerForActivityResult
            }
            // Remember it, so the next run is one tap.
            settings.pushRecent(
                Recent(host.first, host.second, selectedPeer?.hostname ?: "")
            )

            val intent = Intent(this, StreamService::class.java)
                .setAction(StreamService.ACTION_START_SENDER)
                .putExtra(StreamService.EXTRA_PRESET, preset.ordinal)
                .putExtra(StreamService.EXTRA_HOST, host.first)
                .putExtra(StreamService.EXTRA_PORT, host.second)
                .putExtra(StreamService.EXTRA_RESULT_CODE, result.resultCode)
                .putExtra(StreamService.EXTRA_RESULT_DATA, data)
            ContextCompat.startForegroundService(this, intent)
        }

    private val notificationLauncher =
        registerForActivityResult(ActivityResultContracts.RequestPermission()) { }

    /**
     * The microphone switch is only honoured once the permission is
     * granted, and the box unticks itself if it is refused: a switch that
     * looks on while nothing is captured would be worse than no switch.
     */
    private val micPermissionLauncher =
        registerForActivityResult(ActivityResultContracts.RequestPermission()) { granted ->
            if (!granted) {
                micSwitch.isChecked = false
                settings.micEnabled = false
                StreamStats.errorText = getString(R.string.mic_denied)
            }
        }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(R.layout.activity_main)
        settings = Settings(this)
        preset = settings.preset
        applyWindowInsets()
        bindViews()

        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU &&
            ContextCompat.checkSelfPermission(this, Manifest.permission.POST_NOTIFICATIONS)
            != PackageManager.PERMISSION_GRANTED
        ) {
            notificationLauncher.launch(Manifest.permission.POST_NOTIFICATIONS)
        }

        tick = Runnable {
            refresh()
            ui.postDelayed(tick, 150)
        }
    }

    override fun onResume() {
        super.onResume()
        ui.post(tick)
    }

    override fun onPause() {
        super.onPause()
        ui.removeCallbacks(tick)
    }

    override fun onDestroy() {
        discovery.stop()
        super.onDestroy()
    }

    /**
     * Android 15+ draws every app edge to edge whether it asks for it or not,
     * so the layout has to inset itself or the heading ends up behind the
     * status bar and the counters behind the gesture handle.
     */
    private fun applyWindowInsets() {
        val root = findViewById<View>(R.id.root)
        ViewCompat.setOnApplyWindowInsetsListener(root) { view, insets ->
            val bars = insets.getInsets(
                WindowInsetsCompat.Type.systemBars() or WindowInsetsCompat.Type.displayCutout()
            )
            view.setPadding(bars.left, bars.top, bars.right, bars.bottom)
            WindowInsetsCompat.CONSUMED
        }
    }

    private fun bindViews() {
        modeGroup = findViewById(R.id.modeGroup)
        presetGroup = findViewById(R.id.presetGroup)
        presetHint = findViewById(R.id.presetHint)
        receiveBox = findViewById(R.id.receiveBox)
        sendBox = findViewById(R.id.sendBox)
        addressList = findViewById(R.id.addressList)
        peerList = findViewById(R.id.peerList)
        manualIp = findViewById(R.id.manualIp)
        batteryBox = findViewById(R.id.batteryBox)
        batteryButton = findViewById(R.id.batteryButton)
        pinText = findViewById(R.id.pinText)
        pinHint = findViewById(R.id.pinHint)
        newPinButton = findViewById(R.id.newPinButton)
        revokeButton = findViewById(R.id.revokeButton)
        pairBox = findViewById(R.id.pairBox)
        pinInput = findViewById(R.id.pinInput)
        pairButton = findViewById(R.id.pairButton)
        micSwitch = findViewById(R.id.micSwitch)
        micMeterRow = findViewById(R.id.micMeterRow)
        micLabel = findViewById(R.id.micLabel)
        micMeter = findViewById(R.id.micMeter)
        scanButton = findViewById(R.id.scanButton)
        clearButton = findViewById(R.id.clearButton)
        startButton = findViewById(R.id.startButton)
        statusText = findViewById(R.id.statusText)
        outputText = findViewById(R.id.outputText)
        statsText = findViewById(R.id.statsText)
        errorText = findViewById(R.id.errorText)
        estimateText = findViewById(R.id.estimateText)
        statusDot = findViewById(R.id.statusDot)
        meter = findViewById(R.id.meter)

        peerList.adapter = peerAdapter
        peerList.choiceMode = ListView.CHOICE_MODE_SINGLE

        modeGroup.setOnCheckedChangeListener { _, id ->
            mode = if (id == R.id.modeSend) Mode.SEND else Mode.RECEIVE
            applyMode()
        }
        presetGroup.setOnCheckedChangeListener { _, id ->
            preset = when (id) {
                R.id.presetLow -> Latency.LOW
                R.id.presetStable -> Latency.STABLE
                else -> Latency.BALANCED
            }
            presetHint.text = preset.hint
            settings.preset = preset
        }
        micSwitch.isChecked = settings.micEnabled
        micSwitch.setOnCheckedChangeListener { _, checked ->
            settings.micEnabled = checked
            if (checked &&
                ContextCompat.checkSelfPermission(this, Manifest.permission.RECORD_AUDIO)
                != PackageManager.PERMISSION_GRANTED
            ) {
                micPermissionLauncher.launch(Manifest.permission.RECORD_AUDIO)
            }
        }
        newPinButton.setOnClickListener {
            val host = StreamService.sharedPairingHost
            if (host.displayPin() == null) host.openWindow() else host.closeWindow()
        }
        revokeButton.setOnClickListener {
            settings.clearPairings()
            StreamStats.errorText = null
        }
        batteryButton.setOnClickListener { askToRunUnrestricted() }
        pairButton.setOnClickListener { startPairing() }
        scanButton.setOnClickListener { discovery.scan() }
        clearButton.setOnClickListener { clearList() }
        startButton.setOnClickListener { onStartStop() }

        // Restore the preset and the PC used last time.
        presetGroup.check(
            when (preset) {
                Latency.LOW -> R.id.presetLow
                Latency.STABLE -> R.id.presetStable
                Latency.BALANCED -> R.id.presetBalanced
            }
        )
        settings.mostRecent()?.let { entry ->
            manualIp.setText(
                if (entry.port == Config.AUDIO_PORT) entry.host else "${entry.host}:${entry.port}"
            )
        }

        presetHint.text = preset.hint
        applyMode()
    }

    /**
     * Asks Android to stop putting this app to sleep.
     *
     * A foreground service survives the screen going off, but Doze still
     * defers its network traffic and vendor battery managers freeze it
     * outright -- which is why the audio stops a minute after the phone is
     * locked. Only the user can grant the exemption, so this opens the
     * system dialog rather than pretending to fix it.
     */
    private fun askToRunUnrestricted() {
        val intent = Intent(
            android.provider.Settings.ACTION_REQUEST_IGNORE_BATTERY_OPTIMIZATIONS,
            android.net.Uri.parse("package:$packageName")
        )
        runCatching { startActivity(intent) }.onFailure {
            // Some vendors hide that dialog; send them to the app's
            // settings page, where the same switch lives.
            runCatching {
                startActivity(
                    Intent(
                        android.provider.Settings.ACTION_APPLICATION_DETAILS_SETTINGS,
                        android.net.Uri.parse("package:$packageName")
                    )
                )
            }
        }
    }

    /**
     * Proves to the chosen PC that we know the PIN on its screen, and
     * keeps the token it hands back. Runs off the main thread: it waits
     * on the network.
     */
    private fun startPairing() {
        val target = targetHost()
        if (target == null) {
            StreamStats.errorText = "Choose a PC first."
            return
        }
        val pin = Pin.parse(pinInput.text?.toString().orEmpty())
        if (pin == null) {
            StreamStats.errorText = "The PIN is six digits."
            return
        }
        pairButton.isEnabled = false
        StreamStats.errorText = null
        Thread({
            val result = runCatching {
                PairClient.pairWith(java.net.InetAddress.getByName(target.first), pin)
            }.getOrElse { PairClient.Result.Failed(it.message ?: "Pairing failed.") }
            ui.post {
                pairButton.isEnabled = true
                when (result) {
                    is PairClient.Result.Paired -> {
                        settings.addPairing(target.first, result.hostname, result.token)
                        settings.pushRecent(
                            Recent(target.first, target.second, result.hostname)
                        )
                        pinInput.setText("")
                        StreamStats.errorText = null
                    }
                    is PairClient.Result.Failed ->
                        StreamStats.errorText = result.message
                }
            }
        }, "audiopush-pair").apply { isDaemon = true }.start()
    }

    /**
     * Empties the discovered list and forgets the saved PC. Anything still
     * announcing reappears within a second, which is the point: this clears
     * what is stale, not what is live.
     */
    private fun clearList() {
        discovery.clear()
        settings.forgetTarget()
        selectedPeer = null
        selectedRecent = null
        shownTargets = emptyList()
        peerList.clearChoices()
        peerAdapter.clear()
        peerAdapter.notifyDataSetChanged()
        manualIp.setText("")
        StreamStats.errorText = null
    }

    private fun applyMode() {
        val sending = mode == Mode.SEND
        receiveBox.visibility = if (sending) View.GONE else View.VISIBLE
        sendBox.visibility = if (sending) View.VISIBLE else View.GONE
        if (sending) discovery.start() else discovery.stop()
    }

    private fun onStartStop() {
        if (StreamStats.running.get()) {
            StreamService.stop(this)
            StreamStats.running.set(false)
            return
        }
        when (mode) {
            Mode.RECEIVE -> {
                val micWanted = micSwitch.isChecked &&
                    ContextCompat.checkSelfPermission(this, Manifest.permission.RECORD_AUDIO) ==
                    PackageManager.PERMISSION_GRANTED
                val intent = Intent(this, StreamService::class.java)
                    .setAction(StreamService.ACTION_START_RECEIVER)
                    .putExtra(StreamService.EXTRA_PRESET, preset.ordinal)
                    .putExtra(StreamService.EXTRA_MIC, micWanted)
                ContextCompat.startForegroundService(this, intent)
            }
            Mode.SEND -> {
                if (!SenderEngine.isSupported()) {
                    StreamStats.errorText =
                        "Capturing this phone's audio needs Android 10 or newer."
                    return
                }
                val chosen = targetHost()
                if (chosen == null) {
                    StreamStats.errorText = "Choose a PC to send to first."
                    return
                }
                if (settings.tokenFor(chosen.first) == null) {
                    StreamStats.errorText =
                        "Pair with this PC first: type the PIN it shows."
                    return
                }
                val manager =
                    getSystemService(Context.MEDIA_PROJECTION_SERVICE) as MediaProjectionManager
                projectionLauncher.launch(manager.createScreenCaptureIntent())
            }
        }
    }

    /** The PC to stream to: the selected peer, or a typed address. */
    private fun targetHost(): Pair<String, Int>? {
        selectedPeer?.let { return Pair(it.address.hostAddress ?: return null, it.port) }
        selectedRecent?.let { return Pair(it.host, it.port) }
        val typed = manualIp.text?.toString()?.trim().orEmpty()
        if (typed.isEmpty()) return null
        val host = typed.substringBefore(':')
        val port = typed.substringAfter(':', "").toIntOrNull() ?: Config.AUDIO_PORT
        return runCatching { InetAddress.getByName(host) }
            .map { Pair(host, port) }
            .getOrNull()
    }

    private fun refresh() {
        val running = StreamStats.running.get()
        startButton.text = if (running) "STOP" else "START"
        startButton.setBackgroundResource(if (running) R.drawable.btn_stop else R.drawable.btn_start)
        startButton.setTextColor(if (running) 0xFFFFF0EE.toInt() else 0xFF0A1A12.toInt())
        estimateText.text = "~%.0f ms".format(preset.estimateMs())
        modeGroup.isEnabled = !running
        for (i in 0 until presetGroup.childCount) {
            presetGroup.getChildAt(i).isEnabled = !running
        }

        // Addresses to read out to the PC, wired first.
        if (mode == Mode.RECEIVE) {
            val lines = Net.localAddresses().joinToString("\n") {
                "${it.ip.hostAddress}:${Config.AUDIO_PORT}   (${it.label()})"
            }
            addressList.text = if (lines.isEmpty()) {
                "No network connection."
            } else {
                "This phone is \"${Net.deviceName()}\"\n$lines"
            }
        } else {
            val peers = discovery.livePeers()
            // Live receivers first, then ones used before that are not
            // announcing right now: usually they are simply not started
            // yet, and streaming to one works the moment it is.
            val liveHosts = peers.mapNotNull { it.address.hostAddress }.toSet()
            val saved = settings.recent.filterNot { it.host in liveHosts }
            val labels = peers.map { "${it.hostname}  —  ${it.address.hostAddress}" } +
                saved.map { "${it.label()}   (recent)" }
            if (labels != (0 until peerAdapter.count).map { peerAdapter.getItem(it) }) {
                peerAdapter.clear()
                peerAdapter.addAll(labels)
                peerAdapter.notifyDataSetChanged()
            }
            val checked = peerList.checkedItemPosition
            selectedPeer = peers.getOrNull(checked)
            selectedRecent = if (checked >= peers.size) {
                saved.getOrNull(checked - peers.size)
            } else {
                null
            }
        }

        val live = StreamStats.streamLive()
        val dotColour = when {
            !running -> ContextCompat.getColor(this, R.color.text_muted)
            live -> ContextCompat.getColor(this, R.color.green)
            else -> ContextCompat.getColor(this, R.color.amber)
        }
        statusDot.background?.mutate()?.setTint(dotColour)
        statusText.text = when {
            !running -> "Stopped"
            live -> "Streaming — ${StreamStats.peerText ?: ""}"
            mode == Mode.RECEIVE -> "Waiting for a sender on port ${Config.AUDIO_PORT}..."
            else -> "Connecting..."
        }

        // The meter turns amber near clipping, the same as the Windows build.
        val level = StreamStats.level().coerceIn(0f, 1f)
        // A square-root curve so quiet material is still visible.
        meter.progress = (kotlin.math.sqrt(level) * 100).toInt()
        statsText.text = if (running) {
            buildString {
                append("${StreamStats.blocks.get()} blocks")
                if (mode == Mode.RECEIVE) {
                    append(
                        " · buffer %.0f/%.0f ms".format(
                            StreamStats.bufferBlocks.get() * Config.BLOCK_MS,
                            StreamStats.targetBlocks.get() * Config.BLOCK_MS
                        )
                    )
                    append(" · ${StreamStats.dropouts.get()} dropouts")
                }
                val lost = StreamStats.lost.get()
                if (lost > 0) append(" · $lost lost")
                val late = StreamStats.late.get()
                if (late > 0) append(" · $late late")
            }
        } else {
            "Idle"
        }

        // Nag until the app is allowed to run with the screen off. This is
        // the single most common reason a stream dies on a locked phone.
        batteryBox.visibility =
            if (WifiLocks.batteryOptimised(this)) View.VISIBLE else View.GONE

        // Pairing. The PIN only means anything while the receiver is
        // running, because only then is anything listening to answer it.
        if (mode == Mode.RECEIVE) {
            val host = StreamService.sharedPairingHost
            val shownPin = host.displayPin()
            val locked = host.lockedSecondsLeft()
            pinText.text = shownPin ?: ""
            pinText.visibility = if (shownPin == null) View.GONE else View.VISIBLE
            newPinButton.text = getString(
                if (shownPin == null) R.string.pair_new_device else R.string.cancel
            )
            pinHint.text = when {
                locked > 0L -> getString(R.string.pin_locked, locked)
                shownPin != null -> getString(R.string.pin_open, host.windowSecondsLeft())
                running -> getString(R.string.pin_hint)
                else -> getString(R.string.pin_hint_started)
            }
            val paired = settings.pairings.size
            revokeButton.visibility = if (paired > 0) View.VISIBLE else View.GONE
            revokeButton.text = getString(R.string.revoke_all) + " ($paired)"
        } else {
            // Only ask for a PIN when the chosen PC is actually unpaired.
            val chosen = targetHost()
            val needsPairing = chosen != null && settings.tokenFor(chosen.first) == null
            pairBox.visibility = if (needsPairing && !running) View.VISIBLE else View.GONE
        }

        // The microphone meter only appears while it is actually capturing,
        // so an idle switch never looks like a live microphone.
        val micLive = MicStats.running.get() && mode == Mode.RECEIVE
        micMeterRow.visibility = if (micLive) View.VISIBLE else View.GONE
        micSwitch.isEnabled = !running
        if (micLive) {
            micMeter.progress =
                (kotlin.math.sqrt(MicStats.level().coerceIn(0f, 1f)) * 100).toInt()
            // Naming the destination is what stops a hijack being silent.
            val destination = MicStats.targetText
            micLabel.text = if (destination == null) "Mic (waiting)" else "Mic -> $destination"
        }

        // Where playback is actually going. Worth showing: "the meter moves
        // but I hear nothing" is almost always a routing answer.
        val output = StreamStats.outputText
        outputText.visibility = if (output == null || !running) View.GONE else View.VISIBLE
        outputText.text = if (output != null) "Output: $output" else ""

        val error = StreamStats.errorText
        errorText.visibility = if (error == null) View.GONE else View.VISIBLE
        errorText.text = error ?: ""
    }
}
