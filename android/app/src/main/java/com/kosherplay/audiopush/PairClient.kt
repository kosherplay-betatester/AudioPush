package com.kosherplay.audiopush

import java.net.DatagramPacket
import java.net.DatagramSocket
import java.net.InetAddress
import java.net.InetSocketAddress

/**
 * Sender side of pairing: prove knowledge of the PIN shown on the receiver,
 * and collect the token both ends will sign packets with.
 *
 * Blocking and quick: one datagram out, one back, with a short timeout. Call
 * it off the main thread.
 */
object PairClient {
    private const val TIMEOUT_MS = 2500

    sealed class Result {
        data class Paired(val token: Token, val hostname: String) : Result()
        data class Failed(val message: String) : Result()
    }

    fun pairWith(receiver: InetAddress, pin: Pin): Result {
        var socket: DatagramSocket? = null
        try {
            socket = DatagramSocket().apply { soTimeout = TIMEOUT_MS }
            val nonce = Crypto.randomBytes(16)
            val request = Protocol.encodePairRequest(nonce, pin.proof(nonce))
            socket.send(
                DatagramPacket(
                    request,
                    request.size,
                    InetSocketAddress(receiver, Config.DISCOVERY_PORT)
                )
            )

            val buffer = ByteArray(Protocol.DISCOVERY_MAX)
            val deadline = System.currentTimeMillis() + TIMEOUT_MS
            while (System.currentTimeMillis() < deadline) {
                val packet = DatagramPacket(buffer, buffer.size)
                try {
                    socket.receive(packet)
                } catch (_: Exception) {
                    break
                }
                if (packet.address != receiver) continue

                when (val message = Protocol.decodeDiscovery(packet.data, packet.length)) {
                    is Protocol.Discovery.PairAccept -> {
                        // Check the receiver knew the PIN as well. Without
                        // this, a machine that simply answered first could
                        // plant a token and then read everything we send.
                        val context = nonce + message.token.bytes
                        if (!pin.verifyProof(context, message.proof)) {
                            return Result.Failed(
                                "The reply did not come from the device showing that PIN."
                            )
                        }
                        return Result.Paired(message.token, message.hostname)
                    }
                    is Protocol.Discovery.PairReject ->
                        return Result.Failed(Protocol.rejectMessage(message.reason))
                    else -> continue
                }
            }
            return Result.Failed("No answer. Is the receiver started, and on this network?")
        } catch (e: Exception) {
            return Result.Failed("Could not reach the receiver: ${e.message}")
        } finally {
            socket?.close()
        }
    }
}
