package com.unpeel.android

import android.content.Context
import android.content.Intent
import android.os.Bundle
import android.speech.RecognitionListener
import android.speech.RecognizerIntent
import android.speech.SpeechRecognizer

/**
 * Push-to-talk. The caller commits [onFinal] once, through bracketed paste,
 * and does not submit. Cancel drops the utterance.
 */
internal class VoiceCapture(context: Context) {
    private val app = context.applicationContext
    private var recognizer: SpeechRecognizer? = null
    private var latest = ""
    private var delivered = false

    fun available(): Boolean = SpeechRecognizer.isRecognitionAvailable(app)

    fun start(onPartial: (String) -> Unit, onFinal: (String) -> Unit, onError: (String) -> Unit) {
        cancel()
        if (!available()) {
            onError("这台设备没有语音识别")
            return
        }
        delivered = false
        latest = ""
        val speech = SpeechRecognizer.createSpeechRecognizer(app)
        recognizer = speech
        speech.setRecognitionListener(object : RecognitionListener {
            override fun onReadyForSpeech(params: Bundle?) = Unit
            override fun onBeginningOfSpeech() = Unit
            override fun onRmsChanged(rmsdB: Float) = Unit
            override fun onBufferReceived(buffer: ByteArray?) = Unit
            override fun onEndOfSpeech() = Unit
            override fun onError(error: Int) {
                if (error == SpeechRecognizer.ERROR_CLIENT || delivered) return
                if (error == SpeechRecognizer.ERROR_NO_MATCH || error == SpeechRecognizer.ERROR_SPEECH_TIMEOUT) {
                    deliver(latest, onFinal)
                } else {
                    delivered = true
                    onError(voiceError(error))
                }
            }

            override fun onResults(results: Bundle?) {
                val text = results?.getStringArrayList(SpeechRecognizer.RESULTS_RECOGNITION)?.firstOrNull().orEmpty()
                deliver(text.ifEmpty { latest }, onFinal)
            }

            override fun onPartialResults(partialResults: Bundle?) {
                val text = partialResults?.getStringArrayList(SpeechRecognizer.RESULTS_RECOGNITION)?.firstOrNull().orEmpty()
                if (text.isNotEmpty()) {
                    latest = text
                    onPartial(text)
                }
            }

            override fun onEvent(eventType: Int, params: Bundle?) = Unit
        })
        speech.startListening(
            Intent(RecognizerIntent.ACTION_RECOGNIZE_SPEECH).apply {
                putExtra(RecognizerIntent.EXTRA_LANGUAGE_MODEL, RecognizerIntent.LANGUAGE_MODEL_FREE_FORM)
                putExtra(RecognizerIntent.EXTRA_PARTIAL_RESULTS, true)
                putExtra(RecognizerIntent.EXTRA_LANGUAGE, java.util.Locale.getDefault().toLanguageTag())
            },
        )
    }

    fun stop() {
        recognizer?.stopListening()
    }

    fun cancel() {
        delivered = true
        recognizer?.cancel()
        recognizer?.destroy()
        recognizer = null
    }

    private fun deliver(text: String, onFinal: (String) -> Unit) {
        if (delivered) return
        delivered = true
        onFinal(text.trim())
    }
}

private fun voiceError(code: Int): String = when (code) {
    SpeechRecognizer.ERROR_AUDIO -> "麦克风打不开"
    SpeechRecognizer.ERROR_INSUFFICIENT_PERMISSIONS -> "需要麦克风权限"
    SpeechRecognizer.ERROR_NETWORK, SpeechRecognizer.ERROR_NETWORK_TIMEOUT -> "语音识别需要网络"
    else -> "语音识别失败"
}
