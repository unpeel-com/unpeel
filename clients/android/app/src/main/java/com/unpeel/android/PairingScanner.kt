package com.unpeel.android

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.remember
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import androidx.compose.ui.viewinterop.AndroidView
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleEventObserver
import androidx.lifecycle.compose.LocalLifecycleOwner
import com.google.zxing.BarcodeFormat
import com.journeyapps.barcodescanner.BarcodeCallback
import com.journeyapps.barcodescanner.BarcodeResult
import com.journeyapps.barcodescanner.BarcodeView
import com.journeyapps.barcodescanner.DefaultDecoderFactory
import com.journeyapps.barcodescanner.camera.CenterCropStrategy

/**
 * The iOS pairing preview: the camera itself, cropped to fill a rounded
 * rect, with no laser or framing chrome. QR only.
 */
@Composable
fun PairingScanner(
    enabled: Boolean,
    onCode: (String) -> Unit,
) {
    val context = LocalContext.current
    val lifecycle = LocalLifecycleOwner.current.lifecycle
    val preview = remember(context) {
        BarcodeView(context).apply {
            setUseTextureView(true)
            setPreviewScalingStrategy(CenterCropStrategy())
            decoderFactory = DefaultDecoderFactory(listOf(BarcodeFormat.QR_CODE))
        }
    }
    DisposableEffect(preview, enabled, lifecycle) {
        val callback = object : BarcodeCallback {
            override fun barcodeResult(result: BarcodeResult) {
                val text = result.text?.takeIf { it.isNotEmpty() } ?: return
                preview.post { onCode(text) }
            }
        }
        fun running() {
            preview.resume()
            preview.decodeContinuous(callback)
        }
        val observer = LifecycleEventObserver { _, event ->
            when (event) {
                Lifecycle.Event.ON_RESUME -> if (enabled) running()
                Lifecycle.Event.ON_PAUSE -> preview.pause()
                else -> Unit
            }
        }
        lifecycle.addObserver(observer)
        if (enabled && lifecycle.currentState.isAtLeast(Lifecycle.State.RESUMED)) {
            running()
        } else {
            preview.pause()
        }
        onDispose {
            lifecycle.removeObserver(observer)
            preview.pause()
        }
    }
    Box(
        Modifier
            .fillMaxSize()
            .clip(RoundedCornerShape(14.dp))
            .background(Color.Black),
    ) {
        AndroidView(factory = { preview }, modifier = Modifier.fillMaxSize())
    }
}
