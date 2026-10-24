package com.example.rustle

import android.annotation.SuppressLint
import android.bluetooth.*
import android.bluetooth.le.*
import android.content.Context
import android.net.ConnectivityManager
import android.net.Network
import android.net.NetworkCapabilities
import android.net.NetworkRequest
import android.net.wifi.ScanResult as WifiScanResult
import android.net.wifi.WifiManager
import android.net.wifi.WifiNetworkSpecifier
import android.net.wifi.p2p.WifiP2pConfig
import android.net.wifi.p2p.WifiP2pManager
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.os.ParcelUuid
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
    private var advertiser: BluetoothLeAdvertiser? = null
    private var advCallback: AdvertiseCallback? = null
    private var gattServer: BluetoothGattServer? = null
    private var hotspotReservation: WifiManager.LocalOnlyHotspotReservation? = null

    private var p2pManager: WifiP2pManager? = null
    private var p2pChannel: WifiP2pManager.Channel? = null

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
        appContext = context.applicationContext
        try {
            p2pManager = appContext?.getSystemService(Context.WIFI_P2P_SERVICE) as? WifiP2pManager
            p2pChannel = p2pManager?.initialize(appContext, Looper.getMainLooper(), null)
        } catch (e: Throwable) {
            println("WifiP2pManager Init Warnung: ${e.message}")
        }
        try {
            System.loadLibrary("rust_lib_rustle")
            initJni(appContext!!)
        } catch (e: Throwable) {
            println("JNI Init Warnung: ${e.message}")
        }
    }

    private fun ensureP2pInitialized(ctx: Context): Pair<WifiP2pManager, WifiP2pManager.Channel>? {
        if (p2pManager == null) {
            p2pManager = ctx.getSystemService(Context.WIFI_P2P_SERVICE) as? WifiP2pManager
        }
        val mgr = p2pManager ?: return null
        if (p2pChannel == null) {
            p2pChannel = mgr.initialize(ctx, Looper.getMainLooper(), null)
        }
        val ch = p2pChannel ?: return null
        return Pair(mgr, ch)
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

    private fun resolveHotspotIp(excludeIps: Set<String> = emptySet(), preferredInterface: String? = null): String? {
        try {
            val interfaces = NetworkInterface.getNetworkInterfaces()?.toList() ?: return null

            // Falls der Interface-Name aus WifiP2pGroup bekannt ist (z.B. p2p-wlan0-0), prüfe diesen zuerst!
            if (!preferredInterface.isNullOrEmpty()) {
                val pIntf = interfaces.firstOrNull { it.name.equals(preferredInterface, ignoreCase = true) && it.isUp }
                if (pIntf != null) {
                    for (addr in pIntf.inetAddresses) {
                        if (!addr.isLoopbackAddress && addr is Inet4Address) {
                            val ip = addr.hostAddress
                            if (!ip.isNullOrEmpty()) {
                                println("P2P-Interface (direkt via Group) gewählt: ${pIntf.name} -> IP: $ip")
                                return ip
                            }
                        }
                    }
                }
            }

            // Sortiere so, dass P2P-Interfaces (p2p*) und Hotspot-Interfaces (ap*, softap*, swlan*) vor wlan0 stehen
            val sorted = interfaces.sortedBy { intf ->
                val n = intf.name.lowercase()
                when {
                    n.startsWith("p2p") -> 0
                    n.startsWith("ap") || n.startsWith("softap") || n.startsWith("swlan") -> 1
                    n == "wlan2" || n == "wlan1" -> 2
                    n.startsWith("wlan") -> 3
                    else -> 4
                }
            }

            for (intf in sorted) {
                if (!intf.isUp || intf.isLoopback) continue
                val name = intf.name.lowercase()
                if (!name.startsWith("p2p") && !name.startsWith("wlan") && !name.startsWith("ap") && !name.startsWith("swlan") && !name.startsWith("softap")) {
                    continue
                }

                for (addr in intf.inetAddresses) {
                    if (!addr.isLoopbackAddress && addr is Inet4Address) {
                        val ip = addr.hostAddress ?: continue
                        if (!excludeIps.contains(ip)) {
                            println("P2P/Hotspot-Interface gewählt: $name -> IP: $ip")
                            return ip
                        }
                    }
                }
            }
        } catch (e: Exception) {
            println("Fehler beim Ermitteln der P2P/Hotspot-IP: ${e.message}")
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

    /**
     * Startet auf Android einen echten Wi-Fi Direct Autonomous Group Owner via WifiP2pManager.createGroup()!
     * Vorteil: Das normale Heim-WLAN (wlan0) bleibt dabei ungestört verbunden, während auf p2p-wlan0-x
     * ein WPA2-GO (Standard-IP 192.168.49.1) aufgespannt wird.
     * Falls P2P vom Gerät abgelehnt wird, greift automatisch LocalOnlyHotspot als Fallback.
     */
    @JvmStatic
    fun startLocalHotspot(): String {
        val context = appContext ?: return "ERR: Kein Context"
        val cm = context.getSystemService(Context.CONNECTIVITY_SERVICE) as ConnectivityManager

        activeWifiNetwork = null
        try { cm.bindProcessToNetwork(null) } catch (_: Exception) {}

        val existingClientIps = getActiveWifiClientIps()

        // 1. Versuche echten Wi-Fi Direct Group Owner über WifiP2pManager.createGroup() zu starten
        val p2pPair = ensureP2pInitialized(context)
        if (p2pPair != null) {
            val (mgr, ch) = p2pPair
            val p2pFuture = CompletableFuture<String>()

            // Eventuell bestehende alte P2P-Gruppe zuerst aufräumen, dann neue erstellen
            mgr.removeGroup(ch, object : WifiP2pManager.ActionListener {
                override fun onSuccess() {
                    createP2pGroupInternal(mgr, ch, existingClientIps, p2pFuture)
                }

                override fun onFailure(reason: Int) {
                    // Auch wenn keine Gruppe aktiv war (oder Fehler), jetzt neue erstellen
                    createP2pGroupInternal(mgr, ch, existingClientIps, p2pFuture)
                }
            })

            val p2pRes = try {
                p2pFuture.get(12, TimeUnit.SECONDS)
            } catch (e: Exception) {
                "ERR: P2P Timeout"
            }

            if (!p2pRes.startsWith("ERR:")) {
                return p2pRes
            }
            println("WifiP2pManager.createGroup Hinweis ($p2pRes) -> Falle zurück auf LocalOnlyHotspot...")
        }

        // 2. Fallback: LocalOnlyHotspot
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
                            println("LocalOnlyHotspot bereit! SSID=$ssid, PSK=$psk, IP=$finalIp")
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

    private fun createP2pGroupInternal(
        mgr: WifiP2pManager,
        ch: WifiP2pManager.Channel,
        existingClientIps: Set<String>,
        future: CompletableFuture<String>
    ) {
        val createdListener = object : WifiP2pManager.ActionListener {
            override fun onSuccess() {
                // Gruppe wurde erstellt -> Frage nun SSID, Passphrase und Interface-Name ab
                Thread {
                    for (attempt in 1..20) {
                        if (future.isDone) break
                        Thread.sleep(300)
                        mgr.requestGroupInfo(ch) { group ->
                            if (group != null && !group.networkName.isNullOrEmpty() && !group.passphrase.isNullOrEmpty()) {
                                val ssid = group.networkName.removeSurrounding("\"")
                                val psk = group.passphrase
                                val intfName = group.`interface`
                                val ip = resolveHotspotIp(existingClientIps, intfName) ?: "192.168.49.1"
                                currentHotspotIp = ip
                                if (!future.isDone) {
                                    println("Wi-Fi Direct GO bereit! SSID=$ssid, PSK=$psk, Interface=$intfName, IP=$ip (Heim-WLAN bleibt aktiv!)")
                                    future.complete("$ssid;$psk;$ip")
                                }
                            }
                        }
                    }
                    if (!future.isDone) {
                        future.complete("ERR: Konnte P2P GroupInfo nicht auslesen")
                    }
                }.start()
            }

            override fun onFailure(reason: Int) {
                future.complete("ERR: createGroup fehlgeschlagen (Code $reason)")
            }
        }

        // Auf Android 10+ (API 29+) im 2.4-GHz-Band starten für maximale Kompatibilität & Geschwindigkeit
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            try {
                val config = WifiP2pConfig.Builder()
                    .setNetworkName("DIRECT-AndroidGO")
                    .setPassphrase("password1234")
                    .enablePersistentMode(false)
                    .setGroupOperatingBand(WifiP2pConfig.GROUP_OWNER_BAND_2GHZ)
                    .build()
                mgr.createGroup(ch, config, createdListener)
                return
            } catch (_: Exception) {
            }
        }
        mgr.createGroup(ch, createdListener)
    }

    /**
     * Verbindet Android als Client mit dem GO:
     * 1. Zuerst über echten Wi-Fi Direct (WifiP2pConfig.Builder mit SSID + Passphrase):
     *    - Kein System-Dialog / Popup nötig!
     *    - Verbindet über das dedizierte P2P-Interface (p2p-wlan0-x), sodass das normale Heim-WLAN (wlan0)
     *      weiterhin mit dem Internet verbunden bleibt!
     * 2. Falls das fehlschlägt (oder SSID nicht mit "DIRECT-" beginnt), Fallback auf WifiNetworkSpecifier.
     */
    @JvmStatic
    fun connectToSpecifier(ssid: String, psk: String): String {
        val context = appContext ?: return "ERR: Kein Context"
        val cm = context.getSystemService(Context.CONNECTIVITY_SERVICE) as ConnectivityManager
        val wifiManager = context.getSystemService(Context.WIFI_SERVICE) as WifiManager

        println("connectToSpecifier aufgerufen: SSID='$ssid', PSK='$psk'")

        // Falls wir bereits im Ziel-Subnetz (10.42.0.x oder 192.168.49.x) verbunden sind, sofort fertig!
        if (findLocalIpInSameSubnet("10.42.0.1") != null || findLocalIpInSameSubnet("192.168.49.1") != null) {
            println("Bereits über lokales P2P/Subnetz-Interface mit '$ssid' verbunden!")
            return "Erfolgreich per Wi-Fi Direct mit '$ssid' verbunden (Heim-WLAN bleibt aktiv)!"
        }

        // 1. Warte kurz (bis zu 6s), bis der GO-AP im Wi-Fi Scan sichtbar ist
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
                    println("Ziel-GO '$ssid' im Scan gefunden (Versuch $attempt): BSSID=${matchedScan.BSSID}, Freq=${matchedScan.frequency}MHz, Caps=${matchedScan.capabilities}")
                    break
                }
                Thread.sleep(800)
            }
        } catch (_: Exception) {}

        // 2. PRIMÄRER WEG (API 29+): Echte Wi-Fi Direct Verbindung via WifiP2pManager.connect(WifiP2pConfig)
        //    Damit bleibt wlan0 im Heim-WLAN, und es erscheint KEIN nerviger System-Verbindungsdialog!
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q && ssid.startsWith("DIRECT-")) {
            val p2pPair = ensureP2pInitialized(context)
            if (p2pPair != null) {
                val (mgr, ch) = p2pPair
                val p2pFuture = CompletableFuture<String>()

                mgr.removeGroup(ch, object : WifiP2pManager.ActionListener {
                    override fun onSuccess() {
                        connectP2pWithConfig(mgr, ch, ssid, psk, matchedScan, p2pFuture)
                    }

                    override fun onFailure(reason: Int) {
                        connectP2pWithConfig(mgr, ch, ssid, psk, matchedScan, p2pFuture)
                    }
                })

                val p2pRes = try {
                    p2pFuture.get(16, TimeUnit.SECONDS)
                } catch (e: Exception) {
                    "TIMEOUT"
                }

                if (p2pRes == "OK") {
                    // WICHTIG: Bei P2P-Client ist activeWifiNetwork = null, damit sendTcpPacketOverWifi
                    // den Socket direkt an die P2P-Interface-IP (10.42.0.x) bindet und wlan0 im Heim-WLAN lässt!
                    activeWifiNetwork = null
                    try { cm.bindProcessToNetwork(null) } catch (_: Exception) {}
                    return "Erfolgreich per Wi-Fi Direct mit '$ssid' verbunden (Heim-WLAN bleibt aktiv)!"
                }
                println("WifiP2pManager.connect lieferte '$p2pRes' -> Wechsle zu WifiNetworkSpecifier Fallback...")
                mgr.cancelConnect(ch, null)
            }
        }

        // 3. FALLBACK: Reguläres WifiNetworkSpecifier (falls P2P-Interface nicht genutzt werden konnte)
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            activeNetworkCallback?.let {
                try { cm.unregisterNetworkCallback(it) } catch (_: Exception) {}
            }

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
                            future.complete("Erfolgreich per Wi-Fi mit '$ssid' verbunden!")
                        }
                    }.start()
                }

                override fun onLost(network: Network) {
                    if (activeWifiNetwork == network) {
                        activeWifiNetwork = null
                        cm.bindProcessToNetwork(null)
                    }
                }

                override fun onUnavailable() {
                    if (!future.isDone) {
                        future.complete("ERR: Wi-Fi Verbindung abgelehnt oder nicht erreichbar")
                    }
                }
            }

            activeNetworkCallback = callback

            try {
                cm.requestNetwork(request, callback)
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

    @SuppressLint("NewApi")
    private fun connectP2pWithConfig(
        mgr: WifiP2pManager,
        ch: WifiP2pManager.Channel,
        ssid: String,
        psk: String,
        matchedScan: WifiScanResult?,
        future: CompletableFuture<String>
    ) {
        try {
            val builder = WifiP2pConfig.Builder()
                .setNetworkName(ssid)
                .setPassphrase(psk)
                .enablePersistentMode(false)

            if (matchedScan != null && matchedScan.frequency > 0) {
                builder.setGroupOperatingFrequency(matchedScan.frequency)
            } else {
                builder.setGroupOperatingBand(WifiP2pConfig.GROUP_OWNER_BAND_2GHZ)
            }

            val config = builder.build()
            println("Starte WifiP2pManager.connect zu '$ssid' (Freq=${matchedScan?.frequency ?: "2.4GHz"})...")

            mgr.connect(ch, config, object : WifiP2pManager.ActionListener {
                override fun onSuccess() {
                    println("WifiP2pManager.connect initiiert -> Warte auf DHCP-IP im Ziel-Subnetz...")
                    Thread {
                        for (i in 1..45) {
                            if (future.isDone) break
                            Thread.sleep(300)
                            val localP2pIp = findLocalIpInSameSubnet("10.42.0.1")
                                ?: findLocalIpInSameSubnet("192.168.49.1")
                            if (localP2pIp != null) {
                                currentHotspotIp = localP2pIp.hostAddress ?: ""
                                println("Wi-Fi Direct Client verbunden! Lokale P2P-IP: ${localP2pIp.hostAddress}")
                                future.complete("OK")
                                break
                            }
                        }
                        if (!future.isDone) {
                            future.complete("DHCP_TIMEOUT")
                        }
                    }.start()
                }

                override fun onFailure(reason: Int) {
                    println("WifiP2pManager.connect onFailure: Code $reason")
                    if (!future.isDone) {
                        future.complete("ERR_$reason")
                    }
                }
            })
        } catch (e: Exception) {
            println("connectP2pWithConfig Exception: ${e.message}")
            if (!future.isDone) {
                future.complete("EXC: ${e.message}")
            }
        }
    }

    @JvmStatic
    fun sendTcpPacketOverWifi(targetIp: String, port: Int, message: String): String {
        return try {
            val cleanIp = targetIp.trim()
            val targetInet = Inet4Address.getByName(cleanIp)

            // 1. Prüfe zuerst, ob wir ein lokales Interface (z.B. p2p-wlan0-0) im selben /24-Subnetz wie
            //    die Ziel-IP (z.B. 10.42.0.1 oder 192.168.49.x) haben. Wenn ja, binden wir den Socket direkt
            //    an diese lokale P2P-IPv4-Adresse! Dadurch läuft der Traffic garantiert über das P2P-Interface,
            //    während das normale Heim-WLAN (wlan0) unangetastet bleibt!
            val localSubnetAddr = findLocalIpInSameSubnet(cleanIp)
            val clientNet = activeWifiNetwork

            val socket: Socket = if (localSubnetAddr != null && clientNet == null) {
                Socket().also { rawSocket ->
                    try {
                        rawSocket.bind(InetSocketAddress(localSubnetAddr, 0))
                    } catch (e: Exception) {
                        println("Warnung: Konnte Socket nicht an P2P-IP $localSubnetAddr binden: ${e.message}")
                    }
                }
            } else if (clientNet != null) {
                clientNet.socketFactory.createSocket()
            } else {
                Socket().also { rawSocket ->
                    val fallbackBind = localSubnetAddr
                        ?: currentHotspotIp.takeIf { it.isNotEmpty() }?.let { Inet4Address.getByName(it) }
                    if (fallbackBind != null) {
                        try {
                            rawSocket.bind(InetSocketAddress(fallbackBind, 0))
                        } catch (_: Exception) {}
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
            "Erfolg: $reply"
        } catch (e: Throwable) {
            println("TCP Fehler Details: ${e.javaClass.simpleName}: ${e.message}")
            "ERR: ${e.javaClass.simpleName}: ${e.message}"
        }
    }
}
