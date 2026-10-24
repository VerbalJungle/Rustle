package com.example.rustle

import android.Manifest
import android.os.Build
import android.os.Bundle
import androidx.core.app.ActivityCompat
import io.flutter.embedding.android.FlutterActivity

class MainActivity: FlutterActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        NativeWifiBridge.init(this)

        val permissions = mutableListOf(
            Manifest.permission.ACCESS_FINE_LOCATION,
            Manifest.permission.ACCESS_COARSE_LOCATION
        )

        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
            permissions.add(Manifest.permission.BLUETOOTH_SCAN)
            permissions.add(Manifest.permission.BLUETOOTH_ADVERTISE)
            permissions.add(Manifest.permission.BLUETOOTH_CONNECT)
        }

        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            // Zwingend für Hotspot ab Android 13+:
            permissions.add(Manifest.permission.NEARBY_WIFI_DEVICES)
        }

        ActivityCompat.requestPermissions(this, permissions.toTypedArray(), 1001)
    }
}