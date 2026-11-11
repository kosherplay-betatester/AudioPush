package com.kosherplay.audiopush

import android.content.Context
import android.net.wifi.WifiManager
import android.os.Build
import java.net.DatagramPacket
import java.net.DatagramSocket
import java.net.Inet4Address
import java.net.InetAddress
import java.net.InetSocketAddress
import java.net.NetworkInterface
import java.util.concurrent.atomic.AtomicBoolean

/** An IPv4 address on this device, with the interface it belongs to. */
data class LocalAddress(val interfaceName: String, val ip: Inet4Address, val prefixLength: Short) {
    /** Directed broadcast address for this subnet. */
    fun broadcast(): InetAddress {
        val addr = ip.address
        val value = ((addr[0].toInt() and 0xFF) shl 24) or
            ((addr[1].toInt() and 0xFF) shl 16) or
            ((addr[2].toInt() and 0xFF) shl 8) or
            (addr[3].toInt() and 0xFF)
        val mask = if (prefixLength.toInt() == 0) 0 else (-1 shl (32 - prefixLength))
        val broadcast = value or mask.inv()
        return InetAddress.getByAddress(
            byteArrayOf(
                (broadcast ushr 24).toByte(),
                (broadcast ushr 16).toByte(),
                (broadcast ushr 8).toByte(),
                broadcast.toByte()
            )
        )
    }

    /** True for a USB tether or Ethernet link, which is the wired fast path. */
    fun isWired(): Boolean {
        val name = interfaceName.lowercase()
        return name.startsWith("rndis") || name.startsWith("usb") || name.startsWith("eth")
    }

    fun label(): String = when {
        isWired() -> "USB / wired"
        interfaceName.lowercase().startsWith("wlan") -> "Wi-Fi"
        else -> interfaceName
    }
}

/**
 * Where the return microphone stream should go.
 *
 * Empty until the first audio packet arrives: a receiver cannot know who is
 * streaming to it in advance, it finds out when someone does.
 */
class PeerTarget {
    @Volatile
    var address: InetAddress? = null

    /** The pairing the accepted sender authenticated with. */
    @Volatile
    var token: Token? = null
}

object Net {
    /** This device's name, as the receiver advertises it. */
    fun deviceName(): String {
        val model = Build.MODEL ?: "Android"
        val brand = Build.MANUFACTURER ?: ""
        return if (model.startsWith(brand, ignoreCase = true)) model else "$brand $model".trim()
    }

    /**
     * All usable IPv4 addresses, loopback excluded.
     *
     * A USB tether appears here as `rndis0`/`usb0` alongside `wlan0`, which is
     * how the wired path shows up without any special handling: it is simply
     * another address the PC can send to.
     */
    fun localAddresses(): List<LocalAddress> {
        val out = ArrayList<LocalAddress>()
        try {
            for (iface in NetworkInterface.getNetworkInterfaces()) {
                if (!iface.isUp || iface.isLoopback) continue
                // Cellular interfaces carry an address the PC can never reach,
                // and listing them only invites typing the wrong one into the
                // sender.
                val name = iface.name.lowercase()
                if (name.startsWith("rmnet") || name.startsWith("ccmni") ||
                    name.startsWith("pdp") || name.startsWith("clat") ||
                    name.startsWith("dummy")
                ) {
                    continue
                }
                for (ia in iface.interfaceAddresses) {
                    val address = ia.address
                    if (address is Inet4Address && !address.isLinkLocalAddress) {
                        out.add(LocalAddress(iface.name, address, ia.networkPrefixLength))
                    }
                }
            }
        } catch (_: Exception) {
            // A device with no network at all is a normal state, not an error.
        }
        // Wired first: if a USB tether is up, that is the address to use.
        return out.sortedByDescending { it.isWired() }
    }
}

/**
 * Holds the WiFi locks that make low-latency UDP viable on Android.
 *
 * Without the multicast lock the WiFi chip filters broadcast traffic, so
 * discovery never arrives. Without the low-latency WiFi lock the radio sleeps
 * between beacons and delivers packets in late bursts, which is the single
 * biggest cause of hiccups on an otherwise healthy link.
 */
class WifiLocks(context: Context) {
    private val wifi = context.applicationContext
        .getSystemService(Context.WIFI_SERVICE) as WifiManager

    /**
     * Keeps the CPU running with the screen off.
     *
     * A foreground service keeps the process from being killed, but it does
     * not keep the processor awake: with the screen off the threads doing the
     * receiving and the mixing get suspended and the audio stops. This is
     * what holds them running, and it is released the moment streaming ends.
     */
    private val cpuLock = (context.applicationContext
        .getSystemService(Context.POWER_SERVICE) as android.os.PowerManager)
        .newWakeLock(android.os.PowerManager.PARTIAL_WAKE_LOCK, "AudioPush:stream")
    private val multicast = wifi.createMulticastLock("audiopush")
    private val wifiLock = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
        wifi.createWifiLock(WifiManager.WIFI_MODE_FULL_LOW_LATENCY, "audiopush")
    } else {
        @Suppress("DEPRECATION")
        wifi.createWifiLock(WifiManager.WIFI_MODE_FULL_HIGH_PERF, "audiopush")
    }

    fun acquire() {
        runCatching {
            multicast.setReferenceCounted(false)
            multicast.acquire()
        }
        runCatching {
            wifiLock.setReferenceCounted(false)
            wifiLock.acquire()
        }
        runCatching {
            cpuLock.setReferenceCounted(false)
            cpuLock.acquire()
        }
    }

    fun release() {
        runCatching { if (multicast.isHeld) multicast.release() }
        runCatching { if (wifiLock.isHeld) wifiLock.release() }
        runCatching { if (cpuLock.isHeld) cpuLock.release() }
    }

    companion object {
        /**
         * Whether the system will leave this app alone with the screen off.
         *
         * Doze defers network traffic for apps that are not exempt, which is
         * exactly what silences a stream a minute after the phone is locked.
         */
        fun batteryOptimised(context: Context): Boolean {
            val power = context.applicationContext
                .getSystemService(Context.POWER_SERVICE) as android.os.PowerManager
            return !power.isIgnoringBatteryOptimizations(context.packageName)
        }
    }
}

/**
 * Announces this device as an available receiver, once a second, on every
 * interface, and answers scan probes immediately.
 */
class Announcer(
    private val audioPort: Int,
    private val host: PairingHost,
    private val tokens: TokenStore,
    /** Called when a new sender pairs, so the caller can persist it. */
    private val onPaired: (InetAddress, Token) -> Unit
) {
    private val stop = AtomicBoolean(false)
    private var thread: Thread? = null

    fun start() {
        stop.set(false)
        thread = Thread({ run() }, "audiopush-announce").apply {
            isDaemon = true
            start()
        }
    }

    fun stop() {
        stop.set(true)
        thread?.join(500)
        thread = null
    }

    private fun run() {
        var socket: DatagramSocket? = null
        try {
            socket = DatagramSocket(null).apply {
                reuseAddress = true
                broadcast = true
                bind(InetSocketAddress(Config.DISCOVERY_PORT))
                soTimeout = 200
            }
            val payload = Protocol.encodeAnnounce(Net.deviceName(), audioPort)
            val buffer = ByteArray(Protocol.DISCOVERY_MAX)
            var lastAnnounce = 0L

            while (!stop.get()) {
                val now = System.currentTimeMillis()
                if (now - lastAnnounce >= Config.ANNOUNCE_INTERVAL_MS) {
                    lastAnnounce = now
                    broadcastAnnounce(socket, payload)
                }
                try {
                    val packet = DatagramPacket(buffer, buffer.size)
                    socket.receive(packet)
                    when (val message =
                        Protocol.decodeDiscovery(packet.data, packet.length)) {
                        // Answer scan probes straight away.
                        is Protocol.Discovery.Probe -> socket.send(
                            DatagramPacket(payload, payload.size, packet.address, packet.port)
                        )
                        // A sender proving it knows the PIN on screen.
                        is Protocol.Discovery.PairRequest -> {
                            val reply = when (val outcome =
                                host.handleRequest(message.nonce, message.proof)) {
                                is PairingHost.Outcome.Paired -> {
                                    // Trust it at once, so the sender can
                                    // start without a restart, and persist it.
                                    tokens.add(outcome.token)
                                    onPaired(packet.address, outcome.token)
                                    Protocol.encodePairAccept(
                                        outcome.token,
                                        Net.deviceName(),
                                        host.acceptProof(message.nonce, outcome.token)
                                    )
                                }
                                is PairingHost.Outcome.Refused ->
                                    Protocol.encodePairReject(outcome.reason)
                            }
                            socket.send(
                                DatagramPacket(
                                    reply, reply.size, packet.address, packet.port
                                )
                            )
                        }
                        else -> {}
                    }
                } catch (_: Exception) {
                    // Receive timeout: the normal case between probes.
                }
            }
        } catch (e: Exception) {
            StreamStats.errorText = "Discovery unavailable: ${e.message}. " +
                "The PC can still connect by IP."
        } finally {
            socket?.close()
        }
    }

    private fun broadcastAnnounce(socket: DatagramSocket, payload: ByteArray) {
        for (local in Net.localAddresses()) {
            runCatching {
                socket.send(
                    DatagramPacket(
                        payload, payload.size, local.broadcast(), Config.DISCOVERY_PORT
                    )
                )
            }
        }
        runCatching {
            socket.send(
                DatagramPacket(
                    payload,
                    payload.size,
                    InetAddress.getByName("255.255.255.255"),
                    Config.DISCOVERY_PORT
                )
            )
        }
    }
}

/** A Windows receiver this phone has heard announce itself. */
data class Peer(val hostname: String, val address: InetAddress, val port: Int, val lastSeenMs: Long)

/** Listens for receiver announcements so the phone can stream *to* a PC. */
class DiscoveryListener {
    private val stop = AtomicBoolean(false)
    private var thread: Thread? = null
    private val peers = HashMap<String, Peer>()

    @Synchronized
    fun livePeers(): List<Peer> {
        val now = System.currentTimeMillis()
        peers.entries.removeAll { now - it.value.lastSeenMs > Config.PEER_TIMEOUT_MS }
        return peers.values.sortedBy { it.hostname }
    }

    fun start() {
        stop.set(false)
        thread = Thread({ run() }, "audiopush-discover").apply {
            isDaemon = true
            start()
        }
    }

    /**
     * Empties the list. Anything still announcing reappears within a second,
     * which is the point: this clears what is stale, not what is live.
     */
    @Synchronized
    fun clear() {
        peers.clear()
    }

    fun stop() {
        stop.set(true)
        thread?.join(500)
        thread = null
    }

    /** Unicast probe to every host on the local subnets, for blocked broadcast. */
    fun scan() {
        Thread({
            runCatching {
                DatagramSocket().use { socket ->
                    socket.broadcast = true
                    val probe = Protocol.encodeProbe()
                    for (local in Net.localAddresses()) {
                        if (local.prefixLength < 22) continue // too large to sweep
                        val base = local.ip.address
                        for (host in 1..254) {
                            val target = base.copyOf()
                            target[3] = host.toByte()
                            runCatching {
                                socket.send(
                                    DatagramPacket(
                                        probe, probe.size,
                                        InetAddress.getByAddress(target), Config.DISCOVERY_PORT
                                    )
                                )
                            }
                            if (host % 64 == 0) Thread.sleep(2)
                        }
                    }
                }
            }
        }, "audiopush-scan").apply { isDaemon = true }.start()
    }

    private fun run() {
        var socket: DatagramSocket? = null
        try {
            socket = DatagramSocket(null).apply {
                reuseAddress = true
                broadcast = true
                bind(InetSocketAddress(Config.DISCOVERY_PORT))
                soTimeout = 200
            }
            val ownIps = Net.localAddresses().map { it.ip.hostAddress }.toSet()
            val buffer = ByteArray(Protocol.DISCOVERY_MAX)
            while (!stop.get()) {
                try {
                    val packet = DatagramPacket(buffer, buffer.size)
                    socket.receive(packet)
                    val message = Protocol.decodeDiscovery(packet.data, packet.length)
                    if (message is Protocol.Discovery.Announce) {
                        val ip = packet.address.hostAddress ?: continue
                        if (ip in ownIps) continue // our own broadcast
                        synchronized(this) {
                            peers[ip] = Peer(
                                message.hostname, packet.address, message.audioPort,
                                System.currentTimeMillis()
                            )
                        }
                    }
                } catch (_: Exception) {
                    // Timeout.
                }
            }
        } catch (e: Exception) {
            StreamStats.errorText = "Could not listen for receivers: ${e.message}"
        } finally {
            socket?.close()
        }
    }
}
