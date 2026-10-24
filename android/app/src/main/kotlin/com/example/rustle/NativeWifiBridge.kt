package com.example.rustle

import android.annotation.SuppressLint
import android.bluetooth.*
import android.bluetooth.le.*
import android.content.Context
import android.net.ConnectivityManager
import android.net.Network
import android.net.NetworkCapabilities
import android.net.NetworkRequest
import android.net.wifi.WifiManager
import android.net.wifi.WifiNetworkSpecifier
import android.os.Build
import android.os.Handler
import android.os.Looper
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
    private var hotspotReservation: WifiManager.LocalOnlyHotspotReservation? = null

    @Volatile
    private var lastIncomingRequest: String = ""

    @Volatile
    private var currentCredentials: String = ""

    private val SERVICE_UUID = UUID.fromString("4fafc201-1fb5-459e-8fcc-c5c9c331914b")
    private val SYN_CHAR_UUID = UUID.fromString("beb5483e-36e1-4688-b7f5-ea07361b26a8")
    private val ACK_CHAR_UUID = UUID.fromString("8260c653-1a54-426b-9e36-e84c238bc669")
    private val serviceParcelUuid = ParcelUuid(SERVICE_UUID)

    @Volatile
    private var activeWifiNetwork: Network? = null

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

    private fun getLocalHotspotIp(): String {
        try {
            val interfaces = java.net.NetworkInterface.getNetworkInterfaces()
            for (intf in interfaces) {
                if (!intf.isUp || intf.isLoopback) continue
                if (intf.name.startsWith("ap") || intf.name.startsWith("wlan") || intf.name.startsWith("swlan")) {
                    for (addr in intf.inetAddresses) {
                        if (!addr.isLoopbackAddress && addr is java.net.Inet4Address) {
                            return addr.hostAddress ?: "192.168.43.1"
                        }
                    }
                }
            }
        } catch (_: Exception) {
        }
        return "192.168.43.1"
    }

    @JvmStatic
    fun startBleAdvertising(advName: String): String {
        val ctx = appContext ?: return "ERR: Kein Context"
        return try {
            val btManager = ctx.getSystemService(Context.BLUETOOTH_SERVICE) as BluetoothManager
            val adapter = btManager.adapter ?: return "ERR: Kein BT Adapter"

            try {
                adapter.name = advName
            } catch (_: SecurityException) {
            }

            lastIncomingRequest = ""
            currentCredentials = ""
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

                override fun onCharacteristicReadRequest(
                    device: BluetoothDevice,
                    requestId: Int,
                    offset: Int,
                    characteristic: BluetoothGattCharacteristic
                ) {
                    if (characteristic.uuid == ACK_CHAR_UUID) {
                        val bytes = currentCredentials.toByteArray(Charsets.UTF_8)
                        gattServer?.sendResponse(device, requestId, BluetoothGatt.GATT_SUCCESS, 0, bytes)
                    } else {
                        gattServer?.sendResponse(device, requestId, BluetoothGatt.GATT_FAILURE, 0, null)
                    }
                }
            })

            val service = BluetoothGattService(SERVICE_UUID, BluetoothGattService.SERVICE_TYPE_PRIMARY)

            val synChar = BluetoothGattCharacteristic(
                SYN_CHAR_UUID,
                BluetoothGattCharacteristic.PROPERTY_WRITE or BluetoothGattCharacteristic.PROPERTY_WRITE_NO_RESPONSE,
                BluetoothGattCharacteristic.PERMISSION_WRITE
            )
            val ackChar = BluetoothGattCharacteristic(
                ACK_CHAR_UUID,
                BluetoothGattCharacteristic.PROPERTY_READ,
                BluetoothGattCharacteristic.PERMISSION_READ
            )

            service.addCharacteristic(synChar)
            service.addCharacteristic(ackChar)
            gattServer?.addService(service)

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
            private var attempts = 0

            override fun onConnectionStateChange(gatt: BluetoothGatt, status: Int, newState: Int) {
                if (newState == BluetoothProfile.STATE_CONNECTED) {
                    gatt.discoverServices()
                } else if (newState == BluetoothProfile.STATE_DISCONNECTED) {
                    if (!future.isDone) {
                        future.complete("ERR: BLE Verbindung getrennt (Status $status)")
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
                    future.complete("ERR: SYN-Charakteristik auf GO nicht gefunden")
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
                    pollAckCharacteristic(gatt)
                } else {
                    future.complete("ERR: Schreiben von Paket 2 fehlgeschlagen ($status)")
                    gatt.disconnect()
                }
            }

            private fun pollAckCharacteristic(gatt: BluetoothGatt) {
                val ackChar = gatt.getService(SERVICE_UUID)?.getCharacteristic(ACK_CHAR_UUID)
                if (ackChar == null) {
                    future.complete("ERR: ACK-Charakteristik auf GO fehlt")
                    gatt.disconnect()
                    return
                }
                Thread.sleep(500)
                gatt.readCharacteristic(ackChar)
            }

            override fun onCharacteristicRead(
                gatt: BluetoothGatt,
                characteristic: BluetoothGattCharacteristic,
                value: ByteArray,
                status: Int
            ) {
                handleReadResult(gatt, value, status)
            }

            @Deprecated("Deprecated in Java")
            override fun onCharacteristicRead(
                gatt: BluetoothGatt,
                characteristic: BluetoothGattCharacteristic,
                status: Int
            ) {
                @Suppress("DEPRECATION")
                handleReadResult(gatt, characteristic.value ?: ByteArray(0), status)
            }

            private fun handleReadResult(gatt: BluetoothGatt, value: ByteArray, status: Int) {
                if (status == BluetoothGatt.GATT_SUCCESS) {
                    val str = String(value, Charsets.UTF_8)
                    if (str.contains(";")) {
                        future.complete(str)
                        gatt.disconnect()
                        return
                    }
                }
                attempts++
                if (attempts < 25) {
                    pollAckCharacteristic(gatt)
                } else {
                    future.complete("ERR: Timeout beim Warten auf Paket 3 (Hotspot-Daten)")
                    gatt.disconnect()
                }
            }
        }

        device.connectGatt(ctx, false, gattCallback, BluetoothDevice.TRANSPORT_LE)

        return try {
            future.get(20, TimeUnit.SECONDS)
        } catch (e: Exception) {
            "ERR: Timeout: ${e.message}"
        }
    }

    @JvmStatic
    fun pollIncomingBleRequest(): String {
        val req = lastIncomingRequest
        lastIncomingRequest = ""
        return req
    }

    @JvmStatic
    fun setBleHotspotCredentials(creds: String): String {
        currentCredentials = creds
        return "OK"
    }

    @JvmStatic
    fun startLocalHotspot(): String {
        val context = appContext ?: return "ERR: Kein Context"
        val wifiManager = context.getSystemService(Context.WIFI_SERVICE) as WifiManager
        val future = CompletableFuture<String>()

        Handler(Looper.getMainLooper()).post {
            try {
                hotspotReservation?.close()
                wifiManager.startLocalOnlyHotspot(object : WifiManager.LocalOnlyHotspotCallback() {
                    override fun onStarted(reservation: WifiManager.LocalOnlyHotspotReservation) {
                        hotspotReservation = reservation
                        val ssid = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
                            reservation.softApConfiguration.ssid ?: "Android_GO"
                        } else {
                            @Suppress("DEPRECATION")
                            reservation.wifiConfiguration?.SSID ?: "Android_GO"
                        }
                        val psk = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
                            reservation.softApConfiguration.passphrase ?: ""
                        } else {
                            @Suppress("DEPRECATION")
                            reservation.wifiConfiguration?.preSharedKey ?: ""
                        }
                        val ip = getLocalHotspotIp()
                        future.complete("${ssid.removeSurrounding("\"")};$psk;$ip")
                    }

                    override fun onFailed(reason: Int) {
                        future.complete("ERR: Hotspot-Start fehlgeschlagen (Code $reason)")
                    }
                }, Handler(Looper.getMainLooper()))
            } catch (e: Exception) {
                future.complete("ERR: ${e.message}")
            }
        }

        return try {
            future.get(15, TimeUnit.SECONDS)
        } catch (e: Exception) {
            "ERR: Hotspot Timeout: ${e.message}"
        }
    }

    @JvmStatic
    fun connectToSpecifier(ssid: String, psk: String): String {
        val context = appContext ?: return "ERR: Kein Context"
        val connectivityManager = context.getSystemService(Context.CONNECTIVITY_SERVICE) as ConnectivityManager

        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            val specifier = WifiNetworkSpecifier.Builder()
                .setSsid(ssid)
                .setWpa2Passphrase(psk)
                .build()

            val request = NetworkRequest.Builder()
                .addTransportType(NetworkCapabilities.TRANSPORT_WIFI)
                .removeCapability(NetworkCapabilities.NET_CAPABILITY_INTERNET)
                .setNetworkSpecifier(specifier)
                .build()

            val future = CompletableFuture<String>()

            val callback = object : ConnectivityManager.NetworkCallback() {
                override fun onAvailable(network: Network) {
                    activeWifiNetwork = network
                    connectivityManager.bindProcessToNetwork(network)
                    future.complete("Erfolgreich per Wi-Fi mit '$ssid' verbunden!")
                }

                override fun onUnavailable() {
                    future.complete("ERR: Wi-Fi Verbindung vom Nutzer abgelehnt oder Hotspot nicht erreichbar")
                }
            }

            try {
                connectivityManager.requestNetwork(request, callback)
            } catch (e: SecurityException) {
                return "ERR: CHANGE_NETWORK_STATE fehlt im Manifest: ${e.message}"
            } catch (e: Exception) {
                return "ERR: Konnte Wi-Fi Request nicht starten: ${e.message}"
            }

            return try {
                future.get(30, TimeUnit.SECONDS)
            } catch (e: Exception) {
                "ERR: Wi-Fi Timeout: ${e.message}"
            }
        } else {
            return "ERR: Android < 10 nicht unterstützt"
        }
    }

    @JvmStatic
    fun sendTcpPacketOverWifi(targetIp: String, port: Int, message: String): String {
        val ctx = appContext ?: return "ERR: Kein Context"
        val cm = ctx.getSystemService(Context.CONNECTIVITY_SERVICE) as ConnectivityManager

        return try {
            val socket = java.net.Socket()

            // 1. Wenn wir ein gebundenes Specifier-Netzwerk haben: explizit binden
            val net = activeWifiNetwork ?: cm.boundNetworkForProcess
            if (net != null) {
                net.bindSocket(socket)
            } else {
                println("Hinweis: Sende ohne explizites bindSocket (Process-Default)")
            }

            val address = java.net.InetSocketAddress(targetIp, port)
            socket.connect(address, 5000)
            socket.soTimeout = 5000

            val out = socket.getOutputStream()
            out.write(message.toByteArray(Charsets.UTF_8))
            out.flush()

            val input = socket.getInputStream()
            val buf = ByteArray(4096)
            val read = input.read(buf)
            val reply = if (read > 0) String(buf, 0, read, Charsets.UTF_8) else "Keine Antwort"

            socket.close()
            "Erfolg: Antwort vom GO: $reply"
        } catch (e: Exception) {
            "ERR: TCP Senden fehlgeschlagen: ${e.message}"
        }
    }
}