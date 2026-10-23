import 'dart:convert';
import 'dart:io';
import 'dart:math';
import 'package:flutter_blue_plus/flutter_blue_plus.dart';
import 'package:permission_handler/permission_handler.dart';
import 'package:rustle/src/rust/api/wifi.dart';
import 'ble_constants.dart';

class HandshakeManager {
  static final int myWeight = Random().nextInt(10000);
  static final String myDeviceId = "Peer_${Random().nextInt(999)}";

  /// 1. Berechtigungen (nur auf Android nötig)
  static Future<bool> requestPermissions() async {
    if (!Platform.isAndroid) return true;

    final statuses = await [
      Permission.bluetoothScan,
      Permission.bluetoothConnect,
      Permission.bluetoothAdvertise,
      Permission.location,
      Permission.nearbyWifiDevices,
    ].request();

    return statuses.values.every((s) => s.isGranted);
  }

  /// 2. Vollautomatischer Handshake
  static Future<String> startAutoHandshake({
    required Function(String) onStatus,
  }) async {
    onStatus("Prüfe Berechtigungen...");
    if (!await requestPermissions()) {
      throw Exception("Berechtigungen wurden nicht erteilt");
    }

    onStatus("Eigene Priorität: $myWeight. Scanne nach Partnern...");
    await FlutterBluePlus.startScan(
      withServices: [Guid(BleConstants.serviceUuid)],
      timeout: const Duration(seconds: 8),
    );

    BluetoothDevice? targetDevice;

    try {
      final scanResult = await FlutterBluePlus.scanResults
          .expand((results) => results)
          .firstWhere(
            (result) => result.advertisementData.serviceUuids.contains(
          Guid(BleConstants.serviceUuid),
        ),
      )
          .timeout(const Duration(seconds: 8));

      targetDevice = scanResult.device;
    } catch (_) {
      // Kein Partner gefunden -> Zeitüberschreitung
    }

    await FlutterBluePlus.stopScan();

    // FALL A: Es wurde ein Partner gefunden -> Wir verbinden uns und verhandeln
    if (targetDevice != null) {
      onStatus("Partner gefunden: ${targetDevice.platformName}. Verbinde via BLE...");
      await targetDevice.connect(timeout: const Duration(seconds: 5));

      final services = await targetDevice.discoverServices();
      final service = services.firstWhere((s) => s.uuid == Guid(BleConstants.serviceUuid));
      final char = service.characteristics.firstWhere((c) => c.uuid == Guid(BleConstants.credentialsCharUuid));

      // Daten des Partners lesen
      final raw = await char.read();
      final partnerData = jsonDecode(utf8.decode(raw));
      final int partnerWeight = partnerData['weight'] ?? 0;

      onStatus("Partner-Priorität: $partnerWeight (Eigene: $myWeight)");

      if (myWeight > partnerWeight) {
        // WIR haben gewonnen -> Wir starten den Hotspot
        onStatus("Ich werde Hotspot (GO). Starte Wi-Fi...");
        final hotspotInfo = await startHotspot(
          ssid: "Rustle_${Random().nextInt(999)}",
          password: "rustle_secure_pass",
        );

        // Zugangsdaten per BLE an Partner senden
        final payload = jsonEncode({
          "action": "CONNECT",
          "ssid": "Rustle_Direct_GO",
          "psk": "rustle_secure_pass"
        });
        await char.write(utf8.encode(payload));
        await targetDevice.disconnect();

        return "Hotspot aktiv! Partner informiert.\n$hotspotInfo";
      } else {
        // PARTNER hat gewonnen -> Wir warten auf seine Wi-Fi Daten
        onStatus("Partner wird Hotspot. Warte auf Wi-Fi Daten...");

        // Lesen der vom Partner hinterlegten Wi-Fi Daten
        final wifiRaw = await char.read();
        final wifiData = jsonDecode(utf8.decode(wifiRaw));

        final ssid = wifiData['ssid'];
        final psk = wifiData['psk'];

        onStatus("Empfangen: SSID '$ssid'. Verbinde via Rust...");
        await targetDevice.disconnect();

        final result = await connectToNetwork(ssid: ssid, password: psk);
        return "Erfolgreich via Wi-Fi verbunden: $result";
      }
    }
    // FALL B: Niemand gefunden -> Wir bieten uns selbst als Ziel an
    else {
      onStatus("Kein Partner gefunden. Übernehme Host-Rolle...");
      final hotspotInfo = await startHotspot(
        ssid: "Rustle_Direct_GO",
        password: "password1234",
      );
      return "Hotspot gestartet. Warte auf eingehende Verbindungen.\n$hotspotInfo";
    }
  }
}