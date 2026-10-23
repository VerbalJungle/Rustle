package com.example.rustle

import android.annotation.SuppressLint
import android.bluetooth.*
import android.bluetooth.le.*
import android.content.Context
import android.net.ConnectivityManager
import android.net.Network
import android.net.NetworkCapabilities
import android.net.NetworkRequest
import android.net.wifi.WifiNetworkSpecifier
import android.os.Build
import android.os.ParcelUuid
import java.util.UUID
import java.util.concurrent.CompletableFuture
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.TimeUnit

@SuppressLint("MissingPermission")
object NativeWifiBridge {
    private var appContext: Context? = null
    private var advertiser: BluetoothLeAdvertiser? = null
    private var advCallback: AdvertiseCallback? = null
    private var gattServer: BluetoothGattServer? = null

    @Volatile
    private var lastIncomingRequest: String = ""

    private val SERVICE_UUID = UUID.fromString("4fafc201-1fb5-459e-8fcc-c5c9c331914b")
    private val SYN_CHAR_UUID = UUID.fromString("beb5483e-36e1-4688-b7f5-ea07361b26a8")
    private val serviceParcelUuid = ParcelUuid(SERVICE_UUID)

    @JvmStatic
    external fun initJni(context: Context)

    fun init(context: Context) {
        appContext = context.applicationContext
        try {
            System.loadLibrary("rust_lib_rustle")
            initJni(appContext!!)
        } catch (e: Throwable) {
            println("JNI Init Warnung: ${e.message}")
        }
    }

    @JvmStatic
    fun startBleAdvertising(advName: String): String {
        val ctx = appContext ?: return "ERR: Kein Context"
        return try {
            val btManager = ctx.getSystemService(Context.BLUETOOTH_SERVICE) as BluetoothManager
            val adapter = btManager.adapter ?: return "ERR: Kein BT Adapter"

            try {
                adapter.name = advName
            } catch (_: SecurityException) {}

            // 1. GATT-Server starten, um Paket 2 empfangen zu können
            lastIncomingRequest = ""
            gattServer?.close()
            gattServer = btManager.openGattServer(ctx, object : BluetoothGattServerCallback() {
                override fun onCharacteristicWriteRequest(
                    device: BluetoothDevice,
                    requestId: Int,
                    characteristic: BluetoothGattCharacteristic,
                    preparedWrite: Boolean,
                    responseNeeded: Boolean,
                    offset: Int,
                    value: ByteArray?
                ) {
                    if (characteristic.uuid == SYN_CHAR_UUID && value != null) {
                        val msg = String(value, Charsets.UTF_8)
                        println("Android GATT empfing Paket 2: $msg")
                        lastIncomingRequest = msg
                    }
                    if (responseNeeded) {
                        gattServer?.sendResponse(device, requestId, BluetoothGatt.GATT_SUCCESS, 0, null)
                    }
                }
            })

            val service = BluetoothGattService(SERVICE_UUID, BluetoothGattService.SERVICE_TYPE_PRIMARY)
            val synChar = BluetoothGattCharacteristic(
                SYN_CHAR_UUID,
                BluetoothGattCharacteristic.PROPERTY_WRITE or BluetoothGattCharacteristic.PROPERTY_WRITE_NO_RESPONSE,
                BluetoothGattCharacteristic.PERMISSION_WRITE
            )
            service.addCharacteristic(synChar)
            gattServer?.addService(service)

            // 2. Advertising starten
            advertiser = adapter.bluetoothLeAdvertiser ?: return "ERR: BLE Advertising nicht unterstützt"
            advCallback?.let { advertiser?.stopAdvertising(it) }

            val settings = AdvertiseSettings.Builder()
                .setAdvertiseMode(AdvertiseSettings.ADVERTISE_MODE_LOW_LATENCY)
                .setConnectable(true)
                .build()

            val data = AdvertiseData.Builder()
                .setIncludeDeviceName(false)
                .addServiceUuid(serviceParcelUuid)
                .build()

            val scanResponse = AdvertiseData.Builder()
                .setIncludeDeviceName(true)
                .build()

            advCallback = object : AdvertiseCallback() {}
            advertiser?.startAdvertising(settings, data, scanResponse, advCallback)
            "Android GO bereit ('$advName') - Wartet auf Paket 2..."
        } catch (e: Exception) {
            "ERR: ${e.message}"
        }
    }

    @JvmStatic
    fun stopBleAdvertising(): String {
        advCallback?.let { advertiser?.stopAdvertising(it) }
        gattServer?.close()
        gattServer = null
        return "Android BLE Advertising & GATT-Server gestoppt"
    }

    @JvmStatic
    fun scanBlePeers(durationSecs: Int): String {
        val ctx = appContext ?: return "ERR: Kein Context"
        val btManager = ctx.getSystemService(Context.BLUETOOTH_SERVICE) as BluetoothManager
        val scanner = btManager.adapter?.bluetoothLeScanner
            ?: return "ERR: Scanner nicht bereit"

        val found = ConcurrentHashMap<String, String>()
        val callback = object : ScanCallback() {
            override fun onScanResult(callbackType: Int, result: ScanResult) {
                val dev = result.device
                val name = result.scanRecord?.deviceName ?: dev.name ?: "<Unbekannt>"
                val rssi = result.rssi
                found[dev.address] = "${dev.address}|$name|$rssi"
            }
        }

        scanner.startScan(callback)
        Thread.sleep(durationSecs * 1000L)
        scanner.stopScan(callback)

        return found.values.joinToString(System.lineSeparator())
    }

    // Phase 2: Als Client verbinden und Paket 2 ("CONNECT;<Name>") in die Charakteristik schreiben
    @JvmStatic
    fun sendBleConnectionRequest(targetAddress: String, clientName: String): String {
        val ctx = appContext ?: return "ERR: Kein Context"
        val btManager = ctx.getSystemService(Context.BLUETOOTH_SERVICE) as BluetoothManager
        val adapter = btManager.adapter ?: return "ERR: Kein BT Adapter"

        val device = try {
            adapter.getRemoteDevice(targetAddress)
        } catch (e: Exception) {
            return "ERR: Ungültige MAC-Adresse: ${e.message}"
        }

        val future = CompletableFuture<String>()
        val payload = "CONNECT;$clientName".toByteArray(Charsets.UTF_8)

        val gattCallback = object : BluetoothGattCallback() {
            override fun onConnectionStateChange(gatt: BluetoothGatt, status: Int, newState: Int) {
                if (newState == BluetoothProfile.STATE_CONNECTED) {
                    gatt.discoverServices()
                } else if (newState == BluetoothProfile.STATE_DISCONNECTED) {
                    if (!future.isDone) {
                        future.complete("ERR: Verbindung vorzeitig getrennt (Status $status)")
                    }
                    gatt.close()
                }
            }

            override fun onServicesDiscovered(gatt: BluetoothGatt, status: Int) {
                if (status != BluetoothGatt.GATT_SUCCESS) {
                    future.complete("ERR: Service Discovery fehlgeschlagen ($status)")
                    gatt.disconnect()
                    return
                }
                val service = gatt.getService(SERVICE_UUID)
                val synChar = service?.getCharacteristic(SYN_CHAR_UUID)
                if (synChar == null) {
                    future.complete("ERR: Rustle SYN-Charakteristik auf GO nicht gefunden")
                    gatt.disconnect()
                    return
                }

                if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
                    gatt.writeCharacteristic(synChar, payload, BluetoothGattCharacteristic.WRITE_TYPE_DEFAULT)
                } else {
                    @Suppress("DEPRECATION")
                    synChar.value = payload
                    @Suppress("DEPRECATION")
                    gatt.writeCharacteristic(synChar)
                }
            }

            override fun onCharacteristicWrite(
                gatt: BluetoothGatt,
                characteristic: BluetoothGattCharacteristic,
                status: Int
            ) {
                if (status == BluetoothGatt.GATT_SUCCESS) {
                    future.complete("Paket 2 ('CONNECT;$clientName') erfolgreich an $targetAddress gesendet!")
                } else {
                    future.complete("ERR: Schreiben fehlgeschlagen (GATT Status $status)")
                }
                gatt.disconnect()
            }
        }

        device.connectGatt(ctx, false, gattCallback, BluetoothDevice.TRANSPORT_LE)

        return try {
            future.get(10, TimeUnit.SECONDS)
        } catch (e: Exception) {
            "ERR: Timeout oder Fehler beim Senden von Paket 2: ${e.message}"
        }
    }

    @JvmStatic
    fun pollIncomingBleRequest(): String {
        val req = lastIncomingRequest
        lastIncomingRequest = ""
        return req
    }

    @JvmStatic
    fun startLocalHotspot(): String = "Hotspot auf Android gestartet"

    @JvmStatic
    fun connectToSpecifier(ssid: String, psk: String): String = "Stub"
}