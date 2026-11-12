package com.unpeel.controller

import org.bouncycastle.crypto.agreement.X25519Agreement
import org.bouncycastle.crypto.params.X25519PrivateKeyParameters
import org.bouncycastle.crypto.params.X25519PublicKeyParameters
import java.security.SecureRandom

object RelayProtocol {
    const val VERSION = 1
    const val MAX_FRAME_BYTES = 512 * 1024
    const val AEAD_OVERHEAD = 8 + 16
    const val MAX_PLAINTEXT = MAX_FRAME_BYTES - AEAD_OVERHEAD
}

class RelayCryptoException(message: String) : Exception(message)

/**
 * One Link connection. Session keys are HKDF(e2eKey ‖ X25519, clientSalt ‖
 * hostSalt). Nonces are a 4-byte direction tag plus a strictly increasing
 * counter, so a replay fails the AEAD check and the caller drops the socket.
 */
class RelaySession(
    e2eKey: ByteArray,
    sharedSecret: ByteArray,
    clientSalt: ByteArray,
    hostSalt: ByteArray,
    isHost: Boolean,
) {
    private val sendKey: ByteArray
    private val receiveKey: ByteArray
    private val sendTag: ByteArray
    private val receiveTag: ByteArray
    private var sendCounter = 0L
    private var lastReceived = 0L

    init {
        if (e2eKey.size != 32 || sharedSecret.size != 32 || clientSalt.size != 16 || hostSalt.size != 16) {
            throw RelayCryptoException("handshake failed")
        }
        val ikm = e2eKey + sharedSecret
        val salt = clientSalt + hostSalt
        fun derive(info: String) = Hkdf.sha256(ikm, salt, "unpeel-relay-v${RelayProtocol.VERSION}:$info".toByteArray())
        val clientToHost = derive("c2h")
        val hostToClient = derive("h2c")
        sendKey = if (isHost) hostToClient else clientToHost
        receiveKey = if (isHost) clientToHost else hostToClient
        sendTag = (if (isHost) "h2c!" else "c2h!").toByteArray(Charsets.US_ASCII)
        receiveTag = (if (isHost) "c2h!" else "h2c!").toByteArray(Charsets.US_ASCII)
    }

    fun seal(plaintext: ByteArray): ByteArray {
        if (plaintext.size > RelayProtocol.MAX_PLAINTEXT) throw RelayCryptoException("frame too large")
        if (sendCounter == Long.MAX_VALUE) throw RelayCryptoException("counter exhausted")
        sendCounter += 1
        val counter = sendCounter
        val boxed = AesGcm.seal(sendKey, nonce(sendTag, counter), plaintext)
        return u64be(counter) + boxed
    }

    fun open(frame: ByteArray): ByteArray {
        if (frame.size < 8 + 16) throw RelayCryptoException("open failed")
        val counter = u64(frame.copyOfRange(0, 8))
        if (counter <= lastReceived) throw RelayCryptoException("replay detected")
        val plaintext = try {
            AesGcm.open(receiveKey, nonce(receiveTag, counter), frame.copyOfRange(8, frame.size))
        } catch (_: Exception) {
            throw RelayCryptoException("open failed")
        }
        lastReceived = counter
        return plaintext
    }

    private fun nonce(tag: ByteArray, counter: Long): ByteArray = tag + u64be(counter)
}

object RelayHandshake {
    class EphemeralKey(val privateKey: ByteArray, val publicKey: ByteArray)

    fun generateEphemeral(): EphemeralKey {
        val privateKey = X25519PrivateKeyParameters(SecureRandom())
        val publicKey = privateKey.generatePublicKey().encoded
        return EphemeralKey(privateKey.encoded, publicKey)
    }

    fun sharedSecret(privateKey: ByteArray, peerPublic: ByteArray): ByteArray {
        if (privateKey.size != 32 || peerPublic.size != 32) throw RelayCryptoException("handshake failed")
        val agreement = X25519Agreement()
        agreement.init(X25519PrivateKeyParameters(privateKey, 0))
        val out = ByteArray(32)
        agreement.calculateAgreement(X25519PublicKeyParameters(peerPublic, 0), out, 0)
        return out
    }

    fun transcriptMac(
        e2eKey: ByteArray,
        deviceID: String,
        clientSalt: ByteArray,
        hostSalt: ByteArray,
        clientEphemeralPublicKey: ByteArray,
        hostEphemeralPublicKey: ByteArray,
    ): ByteArray {
        val macKey = Hkdf.sha256(
            e2eKey,
            ByteArray(0),
            "unpeel-relay-v${RelayProtocol.VERSION}:handshake-mac".toByteArray(),
        )
        val transcript = ByteArrayBuilder()
        transcript.u32(RelayProtocol.VERSION)
        for (field in listOf(
            deviceID.toByteArray(Charsets.UTF_8),
            clientSalt,
            hostSalt,
            clientEphemeralPublicKey,
            hostEphemeralPublicKey,
        )) {
            transcript.u32(field.size)
            transcript.bytes(field)
        }
        return Hkdf.hmacSha256(macKey, transcript.toByteArray())
    }
}

private class ByteArrayBuilder {
    private var data = ByteArray(64)
    private var size = 0

    fun u32(value: Int) {
        bytes(byteArrayOf((value ushr 24).toByte(), (value ushr 16).toByte(), (value ushr 8).toByte(), value.toByte()))
    }

    fun bytes(value: ByteArray) {
        ensure(value.size)
        value.copyInto(data, size)
        size += value.size
    }

    fun toByteArray() = data.copyOf(size)

    private fun ensure(extra: Int) {
        if (size + extra <= data.size) return
        data = data.copyOf(maxOf(data.size * 2, size + extra))
    }
}

private fun u64be(value: Long): ByteArray = ByteArray(8) { shift ->
    (value ushr (56 - 8 * shift)).toByte()
}

private fun u64(bytes: ByteArray): Long {
    var value = 0L
    for (byte in bytes) value = (value shl 8) or (byte.toLong() and 0xff)
    return value
}
