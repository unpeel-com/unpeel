package com.unpeel.android

import android.app.Activity
import android.app.KeyguardManager
import android.content.Context
import android.content.Intent
import android.hardware.biometrics.BiometricManager
import android.hardware.biometrics.BiometricPrompt
import android.os.Build
import android.os.CancellationSignal
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue

/**
 * Local UI shield. The pairing token stays where the Controller already put it.
 * A failed biometry read falls through to the device credential, so the phone
 * cannot lock its owner out.
 */
class AppLock(context: Context) {
    private val appContext = context.applicationContext
    private val prefs = appContext.getSharedPreferences("unpeel", Context.MODE_PRIVATE)
    var enabled by mutableStateOf(prefs.getBoolean(KEY, false))
        private set
    var locked by mutableStateOf(enabled)
        private set
    var authInFlight by mutableStateOf(false)
        private set
    var lastError by mutableStateOf<String?>(null)
        private set

    private var autoPrompted = false
    private var pending: ((Boolean) -> Unit)? = null
    private var cancelSignal: CancellationSignal? = null

    /** iOS `AppLockManager.methodLabel` — Face ID / Touch ID / screen lock. */
    fun methodLabel(): String {
        val manager = appContext.getSystemService(BiometricManager::class.java) ?: return "screen lock"
        val bio = if (Build.VERSION.SDK_INT >= 30) {
            manager.canAuthenticate(
                BiometricManager.Authenticators.BIOMETRIC_STRONG or
                    BiometricManager.Authenticators.DEVICE_CREDENTIAL,
            )
        } else {
            @Suppress("DEPRECATION")
            manager.canAuthenticate()
        }
        return if (bio == BiometricManager.BIOMETRIC_SUCCESS) "biometrics" else "screen lock"
    }

    fun capabilityAvailable(): Boolean {
        val keyguard = appContext.getSystemService(KeyguardManager::class.java)
        return keyguard?.isDeviceSecure == true
    }

    fun onBackground() {
        if (!enabled || authInFlight) return
        locked = true
        autoPrompted = false
    }

    /** One automatic prompt per foreground. Returns a credential intent on API 26–28. */
    fun onForeground(activity: Activity): Intent? {
        if (!locked || autoPrompted || authInFlight) return null
        autoPrompted = true
        return begin(activity, "Unlock Unpeel") { ok -> if (ok) unlock() }
    }

    fun unlock(activity: Activity): Intent? {
        if (!locked || authInFlight) return null
        return begin(activity, "Unlock Unpeel") { ok -> if (ok) unlock() }
    }

    /** Confirms the method works before the next background depends on it. */
    fun arm(activity: Activity): Intent? {
        if (enabled || authInFlight) return null
        return begin(activity, "Confirm to lock Unpeel when you leave the app") { ok ->
            if (!ok) return@begin
            enabled = true
            locked = false
            prefs.edit().putBoolean(KEY, true).apply()
        }
    }

    fun disarm() {
        enabled = false
        locked = false
        lastError = null
        prefs.edit().putBoolean(KEY, false).apply()
    }

    fun finishCredential(ok: Boolean) {
        authInFlight = false
        val callback = pending
        pending = null
        if (ok) lastError = null
        callback?.invoke(ok)
    }

    private fun unlock() {
        locked = false
        lastError = null
    }

    private fun begin(activity: Activity, reason: String, onResult: (Boolean) -> Unit): Intent? {
        if (authInFlight) return null
        val keyguard = activity.getSystemService(KeyguardManager::class.java)
        if (keyguard?.isDeviceSecure != true) {
            lastError = "This phone has no fingerprint, face, or screen lock"
            onResult(false)
            return null
        }
        authInFlight = true
        lastError = null
        if (Build.VERSION.SDK_INT >= 29) {
            showBiometric(activity, reason, onResult)
            return null
        }
        @Suppress("DEPRECATION")
        val intent = keyguard.createConfirmDeviceCredentialIntent("Unpeel", reason)
        if (intent == null) {
            authInFlight = false
            lastError = "This phone has no screen lock"
            onResult(false)
            return null
        }
        pending = onResult
        return intent
    }

    private fun showBiometric(activity: Activity, reason: String, onResult: (Boolean) -> Unit) {
        val signal = CancellationSignal()
        cancelSignal = signal
        val builder = BiometricPrompt.Builder(activity).setTitle(reason)
        if (Build.VERSION.SDK_INT >= 30) {
            builder.setAllowedAuthenticators(
                BiometricManager.Authenticators.BIOMETRIC_STRONG or BiometricManager.Authenticators.DEVICE_CREDENTIAL,
            )
        } else {
            @Suppress("DEPRECATION")
            builder.setDeviceCredentialAllowed(true)
        }
        builder.build().authenticate(
            signal,
            activity.mainExecutor,
            object : BiometricPrompt.AuthenticationCallback() {
                override fun onAuthenticationSucceeded(result: BiometricPrompt.AuthenticationResult) {
                    authInFlight = false
                    lastError = null
                    onResult(true)
                }

                override fun onAuthenticationError(errorCode: Int, errString: CharSequence) {
                    authInFlight = false
                    val cancelled = errorCode == BiometricPrompt.BIOMETRIC_ERROR_USER_CANCELED ||
                        errorCode == BiometricPrompt.BIOMETRIC_ERROR_CANCELED
                    if (!cancelled) lastError = errString.toString()
                    onResult(false)
                }
            },
        )
    }

    private companion object {
        const val KEY = "unpeel.android.appLockEnabled"
    }
}
