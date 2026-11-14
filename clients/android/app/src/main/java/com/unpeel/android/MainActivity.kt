package com.unpeel.android

import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge

class MainActivity : ComponentActivity() {
    private var controller: Controller? = null

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        val controller = Controller(this)
        this.controller = controller
        setContent { UnpeelApp(controller) }
    }

    override fun onDestroy() {
        // Rotation stays on this activity (configChanges). A real finish must
        // stop the fit watch: the old one keeps posting the grid it measured
        // and fights whatever the next activity fits.
        controller?.close(revertFit = !isChangingConfigurations)
        controller = null
        super.onDestroy()
    }
}
