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
    private var manualHost: String? = null

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
    private lateinit var scanButton: Button
    private lateinit var startButton: Button
    private lateinit var statusText: TextView
    private lateinit var statsText: TextView
    private lateinit var errorText: TextView
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

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(R.layout.activity_main)
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

    private fun bindViews() {
        modeGroup = findViewById(R.id.modeGroup)
        presetGroup = findViewById(R.id.presetGroup)
        presetHint = findViewById(R.id.presetHint)
        receiveBox = findViewById(R.id.receiveBox)
        sendBox = findViewById(R.id.sendBox)
        addressList = findViewById(R.id.addressList)
        peerList = findViewById(R.id.peerList)
        manualIp = findViewById(R.id.manualIp)
        scanButton = findViewById(R.id.scanButton)
        startButton = findViewById(R.id.startButton)
        statusText = findViewById(R.id.statusText)
        statsText = findViewById(R.id.statsText)
        errorText = findViewById(R.id.errorText)
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
        }
        scanButton.setOnClickListener { discovery.scan() }
        startButton.setOnClickListener { onStartStop() }

        presetHint.text = preset.hint
        applyMode()
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
                val intent = Intent(this, StreamService::class.java)
                    .setAction(StreamService.ACTION_START_RECEIVER)
                    .putExtra(StreamService.EXTRA_PRESET, preset.ordinal)
                ContextCompat.startForegroundService(this, intent)
            }
            Mode.SEND -> {
                if (!SenderEngine.isSupported()) {
                    StreamStats.errorText =
                        "Capturing this phone's audio needs Android 10 or newer."
                    return
                }
                if (targetHost() == null) {
                    StreamStats.errorText = "Choose a PC to send to first."
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
            val labels = peers.map { "${it.hostname}  —  ${it.address.hostAddress}" }
            if (labels != (0 until peerAdapter.count).map { peerAdapter.getItem(it) }) {
                peerAdapter.clear()
                peerAdapter.addAll(labels)
                peerAdapter.notifyDataSetChanged()
            }
            val checked = peerList.checkedItemPosition
            selectedPeer = peers.getOrNull(checked)
        }

        val live = StreamStats.streamLive()
        statusText.text = when {
            !running -> "Stopped"
            live -> "Streaming — ${StreamStats.peerText ?: ""}"
            mode == Mode.RECEIVE -> "Waiting for a sender on port ${Config.AUDIO_PORT}..."
            else -> "Connecting..."
        }

        meter.progress = (StreamStats.level().coerceIn(0f, 1f) * 100).toInt()

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
            "~%.0f ms estimated".format(preset.estimateMs())
        }

        val error = StreamStats.errorText
        errorText.visibility = if (error == null) View.GONE else View.VISIBLE
        errorText.text = error ?: ""
    }
}
