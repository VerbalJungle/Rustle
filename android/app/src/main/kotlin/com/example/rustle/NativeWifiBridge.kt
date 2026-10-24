package com.example.rustle

import android.annotation.SuppressLint
import android.bluetooth.*
import android.bluetooth.le.*
import android.content.Context
import android.content.Intent
import android.net.ConnectivityManager
import android.net.Network
import android.net.NetworkCapabilities
import android.net.NetworkRequest
import android.net.wifi.ScanResult as WifiScanResult
import android.net.wifi.WifiManager
import android.net.wifi.WifiNetworkSpecifier
import android.net.wifi.WifiNetworkSuggestion
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.os.ParcelUuid
import android.provider.Settings
import java.net.Inet4Address
import java.net.InetSocketAddress
import java.net.NetworkInterface
import java.net.Socket
import java.util.UUID
import java.util.concurrent.CompletableFuture
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.TimeUnit

@SuppressLint("MissingPermission")
object NativeWifiBridge {
    private var appContext: Context? = null
    private var currentActivity: android.app.Activity? = null
    private var advertiser: BluetoothLeAdvertiser? = null
    private var advCallback: AdvertiseCallback? = null
    private var gattServer: BluetoothGattServer? = null
    private var hotspotReservation: WifiManager.LocalOnlyHotspotReservation? = null

    private var activeNetworkCallback: ConnectivityManager.NetworkCallback? = null

    @Volatile
    private var lastIncomingRequest: String = ""

    @Volatile
    private var currentCredentials: String = ""

    @Volatile
    private var currentHotspotIp: String = ""

    private val SERVICE_UUID = UUID.fromString("4fafc201-1fb5-459e-8fcc-c5c9c331914b")
    private val SYN_CHAR_UUID = UUID.fromString("beb5483e-36e1-4688-b7f5-ea07361b26a8")
    private val ACK_CHAR_UUID = UUID.fromString("8260c653-1a54-426b-9e36-e84c238bc669")
    private val serviceParcelUuid = ParcelUuid(SERVICE_UUID)

    @Volatile
    private var activeWifiNetwork: Network? = null

    @JvmStatic
    external fun initJni(context: Context)

    fun init(context: Context) {
        if (context is android.app.Activity) {
            currentActivity = context
        }
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
                        val fullBytes = currentCredentials.toByteArray(Charsets.UTF_8)
                        val slice = if (offset < fullBytes.size) {
                            fullBytes.copyOfRange(offset, fullBytes.size)
                        } else {
                            ByteArray(0)
                        }
                        gattServer?.sendResponse(device, requestId, BluetoothGatt.GATT_SUCCESS, offset, slice)
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
                println("BLE onConnectionStateChange: status=$status, newState=$newState")
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
                println("BLE onServicesDiscovered: status=$status, services=${gatt.services.map { it.uuid }}")
                if (status != BluetoothGatt.GATT_SUCCESS) {
                    future.complete("ERR: Service Discovery fehlgeschlagen ($status)")
                    gatt.disconnect()
                    return
                }
                val service = gatt.getService(SERVICE_UUID)
                val synChar = service?.getCharacteristic(SYN_CHAR_UUID)
                if (synChar == null) {
                    println("BLE Fehler: SYN-Charakteristik nicht gefunden! Vorhandene Services: ${gatt.services.map { it.uuid }}")
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
                println("BLE onCharacteristicWrite: status=$status")
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
                    println("BLE Read ACK (Versuch $attempts): '$str'")
                    if (str.contains(";")) {
                        future.complete(str)
                        gatt.disconnect()
                        return
                    }
                }
                attempts++
                if (attempts < 30) {
                    pollAckCharacteristic(gatt)
                } else {
                    future.complete("ERR: Timeout beim Warten auf Paket 3 (Hotspot-Daten)")
                    gatt.disconnect()
                }
            }
        }

        device.connectGatt(ctx, false, gattCallback, BluetoothDevice.TRANSPORT_LE)

        return try {
            future.get(25, TimeUnit.SECONDS)
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

    private fun getActiveWifiClientIps(): Set<String> {
        val result = mutableSetOf<String>()
        val ctx = appContext ?: return result
        try {
            val cm = ctx.getSystemService(Context.CONNECTIVITY_SERVICE) as ConnectivityManager
            for (net in cm.allNetworks) {
                val caps = cm.getNetworkCapabilities(net) ?: continue
                if (caps.hasTransport(NetworkCapabilities.TRANSPORT_WIFI)) {
                    val lp = cm.getLinkProperties(net) ?: continue
                    for (la in lp.linkAddresses) {
                        val addr = la.address
                        if (addr is Inet4Address) {
                            addr.hostAddress?.let { result.add(it) }
                        }
                    }
                }
            }
        } catch (_: Exception) {
        }
        return result
    }

    private fun resolveHotspotIp(excludeIps: Set<String> = emptySet()): String? {
        try {
            val interfaces = NetworkInterface.getNetworkInterfaces()?.toList() ?: return null
            for (intf in interfaces) {
                if (!intf.isUp || intf.isLoopback) continue
                val ips = intf.inetAddresses.toList().filterIsInstance<Inet4Address>().mapNotNull { it.hostAddress }
                if (ips.isNotEmpty()) {
                    println("Aktives Interface: ${intf.name} -> $ips")
                }
            }

            val sorted = interfaces.sortedBy { intf ->
                val n = intf.name.lowercase()
                when {
                    n.startsWith("ap") || n.startsWith("softap") || n.startsWith("swlan") -> 0
                    n == "wlan2" || n == "wlan1" || n.startsWith("p2p") -> 1
                    n.startsWith("wlan") -> 2
                    else -> 3
                }
            }

            for (intf in sorted) {
                if (!intf.isUp || intf.isLoopback) continue
                val name = intf.name.lowercase()
                if (!name.startsWith("wlan") && !name.startsWith("ap") && !name.startsWith("swlan") && !name.startsWith("softap") && !name.startsWith("p2p")) {
                    continue
                }

                for (addr in intf.inetAddresses) {
                    if (!addr.isLoopbackAddress && addr is Inet4Address) {
                        val ip = addr.hostAddress ?: continue
                        if (!excludeIps.contains(ip)) {
                            println("Hotspot-Interface gewählt: $name -> IP: $ip")
                            return ip
                        }
                    }
                }
            }

            for (intf in sorted) {
                if (!intf.isUp || intf.isLoopback) continue
                val name = intf.name.lowercase()
                if (name.startsWith("ap") || name.startsWith("swlan") || name.startsWith("softap") || name == "wlan1" || name == "wlan2") {
                    for (addr in intf.inetAddresses) {
                        if (!addr.isLoopbackAddress && addr is Inet4Address) {
                            return addr.hostAddress
                        }
                    }
                }
            }
        } catch (e: Exception) {
            println("Fehler beim Ermitteln der Hotspot-IP: ${e.message}")
        }
        return null
    }

    /**
     * Findet die lokale IPv4-Adresse unseres Geräts, die im selben /24 Subnetz wie targetIp liegt.
     */
    private fun findLocalIpInSameSubnet(targetIp: String): Inet4Address? {
        try {
            val targetParts = targetIp.trim().split(".")
            if (targetParts.size != 4) return null
            val prefix = "${targetParts[0]}.${targetParts[1]}.${targetParts[2]}."

            val interfaces = NetworkInterface.getNetworkInterfaces()?.toList() ?: return null
            for (intf in interfaces) {
                if (!intf.isUp || intf.isLoopback) continue
                for (addr in intf.inetAddresses) {
                    if (!addr.isLoopbackAddress && addr is Inet4Address) {
                        val hostAddr = addr.hostAddress ?: continue
                        if (hostAddr.startsWith(prefix)) {
                            return addr
                        }
                    }
                }
            }
        } catch (_: Exception) {
        }
        return null
    }

    @JvmStatic
    fun startLocalHotspot(): String {
        val context = appContext ?: return "ERR: Kein Context"
        val wifiManager = context.getSystemService(Context.WIFI_SERVICE) as WifiManager
        val cm = context.getSystemService(Context.CONNECTIVITY_SERVICE) as ConnectivityManager

        activeWifiNetwork = null
        try { cm.bindProcessToNetwork(null) } catch (_: Exception) {}

        val existingClientIps = getActiveWifiClientIps()
        println("startLocalHotspot: Ignoriere bestehende Heim-WLAN IPs: $existingClientIps")

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

                        Thread {
                            var ip: String? = null
                            var attempts = 0
                            while (ip == null && attempts < 15) {
                                Thread.sleep(250)
                                ip = resolveHotspotIp(existingClientIps)
                                attempts++
                            }
                            val finalIp = ip ?: resolveHotspotIp(emptySet()) ?: "192.168.43.1"
                            currentHotspotIp = finalIp
                            println("Hotspot bereit! SSID=$ssid, PSK=$psk, IP=$finalIp")
                            future.complete("${ssid.removeSurrounding("\"")};$psk;$finalIp")
                        }.start()
                    }

                    override fun onFailed(reason: Int) {
                        future.complete("ERR: Hotspot Fehler $reason")
                    }
                }, Handler(Looper.getMainLooper()))
            } catch (e: Exception) {
                future.complete("ERR: ${e.message}")
            }
        }

        return try {
            future.get(15, TimeUnit.SECONDS)
        } catch (e: Exception) {
            "ERR: Hotspot Timeout"
        }
    }

    /**
     * Prüft, ob ein bestimmtes Android Network bereits eine IPv4 im 10.42.0.x (oder Ziel-Subnetz) hat.
     */
    private fun findConnectedLinuxGoNetwork(cm: ConnectivityManager): Network? {
        for (net in cm.allNetworks) {
            val caps = cm.getNetworkCapabilities(net) ?: continue
            if (caps.hasTransport(NetworkCapabilities.TRANSPORT_WIFI)) {
                val lp = cm.getLinkProperties(net) ?: continue
                for (la in lp.linkAddresses) {
                    val addr = la.address
                    if (addr is Inet4Address && addr.hostAddress?.startsWith("10.42.") == true) {
                        return net
                    }
                }
            }
        }
        return null
    }

    @JvmStatic
    fun connectToSpecifier(ssid: String, psk: String): String {
        val context = appContext ?: return "ERR: Kein Context"
        val cm = context.getSystemService(Context.CONNECTIVITY_SERVICE) as ConnectivityManager
        val wifiManager = context.getSystemService(Context.WIFI_SERVICE) as WifiManager

        println("connectToSpecifier aufgerufen: SSID='$ssid', PSK='$psk'")

        // Falls das Smartphone bereits mit dem Linux-Hotspot (10.42.0.x) verbunden ist, sofort fertig!
        findConnectedLinuxGoNetwork(cm)?.let { existingNet ->
            activeWifiNetwork = existingNet
            cm.bindProcessToNetwork(existingNet)
            println("Bereits mit Linux-GO Netzwerk ($existingNet) verbunden!")
            return "Erfolgreich per Wi-Fi mit '$ssid' verbunden!"
        }

        // 1. Aktiv scannen, bis der Ziel-AP in der Android Scanliste erscheint
        var matchedScan: WifiScanResult? = null
        try {
            for (attempt in 1..6) {
                @Suppress("DEPRECATION")
                wifiManager.startScan()
                val results = wifiManager.scanResults ?: emptyList()
                matchedScan = results.firstOrNull {
                    @Suppress("DEPRECATION")
                    val rSsid = it.SSID?.removeSurrounding("\"") ?: ""
                    rSsid == ssid
                }
                if (matchedScan != null) {
                    println("Ziel-AP '$ssid' im Wi-Fi Scan gefunden (Versuch $attempt): BSSID=${matchedScan.BSSID}, Capabilities=${matchedScan.capabilities}, Freq=${matchedScan.frequency}")
                    break
                }
                Thread.sleep(800)
            }
        } catch (e: Exception) {
            println("Warnung beim Vorab-Scan: ${e.message}")
        }

        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            activeNetworkCallback?.let {
                try { cm.unregisterNetworkCallback(it) } catch (_: Exception) {}
            }

            // Versuch A: WifiNetworkSpecifier OHNE setBssid (nur SSID + Passphrase, exakt wie ursprünglich)
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
                    val bound = cm.bindProcessToNetwork(network)
                    println("onAvailable: Netzwerk=$network, bindProcess=$bound")
                    Thread {
                        for (i in 1..25) {
                            val lp = cm.getLinkProperties(network)
                            val hasIpv4 = lp?.linkAddresses?.any { it.address is Inet4Address } == true
                            if (hasIpv4) {
                                println("DHCP IPv4 bereit nach ${i * 150}ms: ${lp?.linkAddresses}")
                                break
                            }
                            Thread.sleep(150)
                        }
                        if (!future.isDone) {
                            future.complete("OK")
                        }
                    }.start()
                }

                override fun onLost(network: Network) {
                    println("onLost: Netzwerk $network getrennt")
                    if (activeWifiNetwork == network) {
                        activeWifiNetwork = null
                        cm.bindProcessToNetwork(null)
                    }
                }

                override fun onUnavailable() {
                    println("onUnavailable: WifiNetworkSpecifier nicht verfügbar")
                    if (!future.isDone) {
                        future.complete("UNAVAILABLE")
                    }
                }
            }

            activeNetworkCallback = callback

            try {
                cm.requestNetwork(request, callback)
            } catch (e: Exception) {
                future.complete("ERR: ${e.message}")
            }

            val specRes = try {
                future.get(15, TimeUnit.SECONDS)
            } catch (e: Exception) {
                "TIMEOUT"
            }

            if (specRes == "OK") {
                return "Erfolgreich per Wi-Fi mit '$ssid' verbunden!"
            }

            // Versuch B (Fallback für Android 11–16, wenn WifiNetworkSpecifier wegen SAE/WPA2-Transition
            // oder gecachtem Ablehnungs-Status sofort onUnavailable liefert):
            // Wir übergeben das WLAN per WifiNetworkSuggestion + Settings.ACTION_WIFI_ADD_NETWORKS Dialog
            // und lauschen auf das neue Wi-Fi-Netzwerk!
            println("WifiNetworkSpecifier lieferte '$specRes' -> Nutze WifiNetworkSuggestion + System-Add-Network Fallback...")
            try { cm.unregisterNetworkCallback(callback) } catch (_: Exception) {}

            val fallbackFuture = CompletableFuture<String>()
            val generalWifiReq = NetworkRequest.Builder()
                .addTransportType(NetworkCapabilities.TRANSPORT_WIFI)
                .removeCapability(NetworkCapabilities.NET_CAPABILITY_INTERNET)
                .build()

            val generalCallback = object : ConnectivityManager.NetworkCallback() {
                override fun onAvailable(network: Network) {
                    Thread {
                        for (i in 1..25) {
                            val lp = cm.getLinkProperties(network)
                            val ipv4 = lp?.linkAddresses?.mapNotNull { it.address as? Inet4Address }
                                ?.firstOrNull { it.hostAddress?.startsWith("10.42.") == true }
                            if (ipv4 != null) {
                                println("Fallback Wi-Fi verbunden mit 10.42.x.x ($network -> $ipv4)!")
                                activeWifiNetwork = network
                                cm.bindProcessToNetwork(network)
                                if (!fallbackFuture.isDone) {
                                    fallbackFuture.complete("OK")
                                }
                                break
                            }
                            Thread.sleep(200)
                        }
                    }.start()
                }
            }

            activeNetworkCallback = generalCallback
            try {
                cm.registerNetworkCallback(generalWifiReq, generalCallback)
            } catch (_: Exception) {}

            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
                try {
                    val sugWpa2 = WifiNetworkSuggestion.Builder()
                        .setSsid(ssid)
                        .setWpa2Passphrase(psk)
                        .setIsAppInteractionRequired(false)
                        .build()
                    val sugWpa3 = WifiNetworkSuggestion.Builder()
                        .setSsid(ssid)
                        .setWpa3Passphrase(psk)
                        .setIsAppInteractionRequired(false)
                        .build()

                    wifiManager.removeNetworkSuggestions(listOf(sugWpa2, sugWpa3))
                    val status = wifiManager.addNetworkSuggestions(listOf(sugWpa2, sugWpa3))
                    println("WifiNetworkSuggestions hinzugefügt (Status=$status)")

                    // Öffne zusätzlich das offizielle Android-System-Popup zum Speichern & Verbinden des Netzwerks
                    currentActivity?.let { act ->
                        val bundle = Bundle().apply {
                            putParcelableArrayList(
                                Settings.EXTRA_WIFI_NETWORK_LIST,
                                arrayListOf(sugWpa2)
                            )
                        }
                        val intent = Intent(Settings.ACTION_WIFI_ADD_NETWORKS).apply {
                            putExtras(bundle)
                        }
                        act.runOnUiThread {
                            try {
                                act.startActivityForResult(intent, 2002)
                            } catch (e: Exception) {
                                println("Konnte ACTION_WIFI_ADD_NETWORKS nicht starten: ${e.message}")
                            }
                        }
                    }
                } catch (e: Exception) {
                    println("Suggestion Fehler: ${e.message}")
                }
            }

            // Prüfe bis zu 25 Sekunden, ob das Interface die 10.42.0.x IP erhält
            for (i in 1..50) {
                if (fallbackFuture.isDone) break
                val net = findConnectedLinuxGoNetwork(cm)
                if (net != null) {
                    activeWifiNetwork = net
                    cm.bindProcessToNetwork(net)
                    fallbackFuture.complete("OK")
                    break
                }
                if (findLocalIpInSameSubnet("10.42.0.1") != null) {
                    fallbackFuture.complete("OK")
                    break
                }
                Thread.sleep(500)
            }

            if (fallbackFuture.isDone && fallbackFuture.getNow("") == "OK") {
                return "Erfolgreich per Wi-Fi mit '$ssid' verbunden!"
            }

            return "ERR: Wi-Fi Verbindung zu '$ssid' fehlgeschlagen ($specRes)"
        } else {
            return "ERR: Android < 10 nicht unterstützt"
        }
    }

    @JvmStatic
    fun sendTcpPacketOverWifi(targetIp: String, port: Int, message: String): String {
        val ctx = appContext ?: return "ERR: Kein Context"
        val cm = ctx.getSystemService(Context.CONNECTIVITY_SERVICE) as ConnectivityManager

        return try {
            val cleanIp = targetIp.trim()
            val targetInet = Inet4Address.getByName(cleanIp)

            // Falls activeWifiNetwork leer ist, aber wir mit 10.42.0.x (Linux-GO) verbunden sind, hole das Network-Objekt
            var clientNet = activeWifiNetwork
            if (clientNet == null && cleanIp.startsWith("10.42.")) {
                clientNet = findConnectedLinuxGoNetwork(cm)
            }

            val socket: Socket = if (clientNet != null) {
                println("TCP Senden (als Android-Client über Network $clientNet) an $cleanIp:$port")
                clientNet.socketFactory.createSocket()
            } else {
                Socket().also { rawSocket ->
                    val localBindAddr = findLocalIpInSameSubnet(cleanIp)
                        ?: currentHotspotIp.takeIf { it.isNotEmpty() }?.let { Inet4Address.getByName(it) }
                    println("TCP Senden (lokaler Bind=$localBindAddr) an $cleanIp:$port")
                    if (localBindAddr != null) {
                        try {
                            rawSocket.bind(InetSocketAddress(localBindAddr, 0))
                        } catch (e: Exception) {
                            println("Warnung: Konnte Socket nicht an $localBindAddr binden: ${e.message}")
                        }
                    }
                }
            }

            socket.soTimeout = 5000
            socket.connect(InetSocketAddress(targetInet, port), 5000)

            val out = socket.getOutputStream()
            out.write(message.toByteArray(Charsets.UTF_8))
            out.flush()

            val input = socket.getInputStream()
            val buf = ByteArray(4096)
            val read = input.read(buf)
            val reply = if (read > 0) String(buf, 0, read, Charsets.UTF_8) else "OK"

            socket.close()
            println("TCP Senden erfolgreich! Antwort: $reply")
            "Erfolg: $reply"
        } catch (e: Throwable) {
            println("TCP Fehler Details: ${e.javaClass.simpleName}: ${e.message}")
            "ERR: ${e.javaClass.simpleName}: ${e.message}"
        }
    }
}
