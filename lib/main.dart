import 'dart:async';
import 'dart:io';
import 'dart:ui';
import 'package:file_picker/file_picker.dart';
import 'package:flutter/material.dart';
import 'package:path_provider/path_provider.dart';
import 'package:rustle/src/rust/api/wifi.dart';
import 'package:rustle/src/rust/frb_generated.dart';
import 'package:rustle/src/rust/platform.dart';
import 'package:rustle/src/rust/transfer/progress.dart';

Future<void> main() async {
  WidgetsFlutterBinding.ensureInitialized();
  await RustLib.init();
  runApp(const MaterialApp(home: BleDiagnosticPage()));
}

class BleDiagnosticPage extends StatefulWidget {
  const BleDiagnosticPage({super.key});

  @override
  State<BleDiagnosticPage> createState() => _BleDiagnosticPageState();
}

class _BleDiagnosticPageState extends State<BleDiagnosticPage> {
  String _status = 'Noch nicht initialisiert';
  String? _goEventBanner;
  String? _activeVpn;
  List<DiscoveredPeer> _peers = [];
  bool _busy = false;
  Timer? _pollTimer;
  TransferProgress? _activeProgress;
  late final AppLifecycleListener _lifecycleListener;

  @override
  void initState() {
    super.initState();
    _lifecycleListener = AppLifecycleListener(
      onExitRequested: () async {
        _pollTimer?.cancel();
        try {
          await bleStopBroadcast();
        } catch (_) {}
        return AppExitResponse.exit;
      },
      onDetach: () {
        _pollTimer?.cancel();
        bleStopBroadcast();
      },
    );
    _initCore();
  }

  @override
  void dispose() {
    _pollTimer?.cancel();
    _lifecycleListener.dispose();
    bleStopBroadcast();
    super.dispose();
  }

  Future<void> _initCore() async {
    try {
      final res = await initCore();
      setState(() => _status = res);
      await _initDownloadDir();
      await _checkVpn();
    } catch (e) {
      setState(() => _status = 'Init-Fehler: $e');
    }
  }

  Future<void> _checkVpn() async {
    try {
      final vpn = await checkActiveVpn();
      if (mounted && vpn != _activeVpn) {
        setState(() => _activeVpn = vpn);
      }
    } catch (e) {
      debugPrint('VPN check error: $e');
    }
  }

  Future<void> _initDownloadDir() async {
    try {
      Directory? dir;
      if (Platform.isAndroid) {
        dir = await getExternalStorageDirectory();
      } else {
        dir = await getDownloadsDirectory();
      }
      dir ??= await getApplicationDocumentsDirectory();
      await setDownloadDirectory(dirPath: dir.path);
    } catch (e) {
      debugPrint('Download dir error: $e');
    }
  }

  void _startPolling({required bool isGo}) {
    _pollTimer?.cancel();
    int tick = 0;
    _pollTimer = Timer.periodic(const Duration(milliseconds: 250), (_) async {
      tick++;
      try {
        // Alle ~2 Sekunden VPN-Status prüfen
        if (tick % 8 == 0) {
          _checkVpn();
        }

        // Nur solange polln, bis der GO den Handshake empfangen hat
        if (isGo && _goEventBanner == null) {
          final event = await blePollAndHandleGo();
          if (event != null && mounted) {
            setState(() {
              _goEventBanner = event;
              _status = 'GO: Hotspot & TCP-Server (:4242/:4243) laufen!';
            });
          }
        }
        final tcpMsg = await pollReceivedPackets();
        if (tcpMsg != null && mounted) {
          setState(() {
            _goEventBanner = 'NACHRICHT EMPFANGEN:\n$tcpMsg';
            _status = 'Letztes Paket erfolgreich verarbeitet!';
          });
        }
        final progress = await pollTransferProgress();
        if (progress != null && mounted) {
          setState(() {
            _activeProgress = progress;
          });
        }
      } catch (e) {
        if (mounted) setState(() => _status = 'Poll-Fehler: $e');
      }
    });
  }

  Future<void> _advertise(bool isGo) async {
    setState(() {
      _busy = true;
      _goEventBanner = null;
      _activeProgress = null;
      _status = 'Starte GO (BLE Advertising + Wi-Fi Direct Hotspot Pre-Warming)...';
    });
    try {
      final res = await bleStartBroadcast(
        deviceName: isGo ? 'Host' : 'Client',
        isGo: isGo,
      );
      setState(() => _status = res);

      if (isGo) {
        _startPolling(isGo: true);
      }
    } catch (e) {
      setState(() => _status = 'Broadcast-Fehler: $e');
    } finally {
      setState(() => _busy = false);
    }
  }

  Future<void> _resetConnection() async {
    _pollTimer?.cancel();
    setState(() {
      _busy = true;
      _goEventBanner = null;
      _activeProgress = null;
      _status = 'Setze Hotspot & BLE zurück (stelle Normal-WLAN wieder her)...';
    });
    try {
      final res = await bleStopBroadcast();
      setState(() {
        _peers = [];
        _status = res;
      });
      await _checkVpn();
    } catch (e) {
      setState(() => _status = 'Reset-Fehler: $e');
    } finally {
      setState(() => _busy = false);
    }
  }

  Future<void> _scan() async {
    setState(() {
      _busy = true;
      _status = 'Scanne nach Rustle-GO (Smart Early-Exit)...';
    });
    try {
      final found = await bleScan(durationSecs: BigInt.from(3));
      setState(() {
        _peers = found;
        _status = 'Scan beendet: ${found.length} Geräte. Tippe auf einen GO zum Verbinden!';
      });
      await _checkVpn();
    } catch (e) {
      setState(() => _status = 'Scan-Fehler: $e');
    } finally {
      setState(() => _busy = false);
    }
  }

  Future<void> _startFullHandshake(DiscoveredPeer peer) async {
    setState(() {
      _busy = true;
      _activeProgress = null;
      _status = '1/3 Sende Paket 2 an ${peer.name}...\n2/3 Lese Paket 3...\n3/3 Verbinde Wi-Fi Direct...';
    });
    try {
      final res = await bleHandshakeAndJoinWifi(
        targetAddress: peer.address,
        clientName: 'PeerClient',
      );
      setState(() => _status = 'ERFOLG!\n$res');
      _startPolling(isGo: false);
    } catch (e) {
      setState(() => _status = 'Handshake-Fehler:\n$e');
    } finally {
      setState(() => _busy = false);
    }
  }

  Future<void> _pickAndSendFile() async {
    final result = await FilePicker.platform.pickFiles();
    if (result == null || result.files.isEmpty) return;

    final filePath = result.files.single.path;
    if (filePath == null) return;

    setState(() {
      _busy = true;
      _activeProgress = null;
      _status = 'Starte Transfer von "${result.files.single.name}" (:4243)...';
    });

    try {
      final res = await startFileTransfer(filePath: filePath);
      setState(() => _status = res);
    } catch (e) {
      setState(() => _status = 'Transfer-Fehler: $e');
    } finally {
      setState(() => _busy = false);
    }
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(
        title: const Text('Rustle: Wi-Fi Direct File Sharing'),
        actions: [
          IconButton(
            tooltip: 'Hotspot & BLE zurücksetzen (Normal-WLAN wiederherstellen)',
            icon: const Icon(Icons.restore),
            onPressed: _busy ? null : _resetConnection,
          ),
        ],
      ),
      body: Padding(
        padding: const EdgeInsets.all(16),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            // VPN-Warnhinweis oben am Bildschirm
            if (_activeVpn != null)
              Container(
                padding: const EdgeInsets.symmetric(horizontal: 14, vertical: 10),
                margin: const EdgeInsets.only(bottom: 12),
                decoration: BoxDecoration(
                  color: Colors.amber.shade100,
                  border: Border.all(color: Colors.amber.shade700, width: 1.5),
                  borderRadius: BorderRadius.circular(8),
                ),
                child: Row(
                  children: [
                    Icon(Icons.warning_amber_rounded, color: Colors.amber.shade900, size: 28),
                    const SizedBox(width: 12),
                    Expanded(
                      child: Column(
                        crossAxisAlignment: CrossAxisAlignment.start,
                        children: [
                          Text(
                            'Achtung: Aktives VPN erkannt ($_activeVpn)!',
                            style: TextStyle(
                              fontWeight: FontWeight.bold,
                              color: Colors.amber.shade900,
                              fontSize: 14,
                            ),
                          ),
                          const SizedBox(height: 2),
                          const Text(
                            'Ein aktives VPN kann den lokalen Wi-Fi Direct Datenverkehr blockieren. '
                            'Falls der Verbindungsaufbau oder TCP-Ping fehlschlägt, bitte das VPN oder dessen Killswitch vorübergehend pausieren.',
                            style: TextStyle(fontSize: 12, color: Colors.black87),
                          ),
                        ],
                      ),
                    ),
                  ],
                ),
              ),

            Row(
              children: [
                Expanded(
                  child: ElevatedButton(
                    onPressed: _busy ? null : () => _advertise(true),
                    child: const Text('1. Als GO warten'),
                  ),
                ),
                const SizedBox(width: 8),
                Expanded(
                  child: ElevatedButton(
                    onPressed: _busy ? null : _scan,
                    child: const Text('2. Als Client scannen'),
                  ),
                ),
                const SizedBox(width: 8),
                ElevatedButton.icon(
                  style: ElevatedButton.styleFrom(backgroundColor: Colors.red.shade100),
                  onPressed: _busy ? null : _resetConnection,
                  icon: const Icon(Icons.power_settings_new, size: 18),
                  label: const Text('Reset'),
                ),
              ],
            ),
            const SizedBox(height: 12),
            if (_goEventBanner != null)
              Container(
                padding: const EdgeInsets.all(12),
                margin: const EdgeInsets.only(bottom: 12),
                color: Colors.green.shade100,
                child: Text(
                  _goEventBanner!,
                  style: const TextStyle(fontWeight: FontWeight.bold, color: Colors.green),
                ),
              ),
            Container(
              padding: const EdgeInsets.all(10),
              color: Colors.black12,
              child: Text(_status, style: const TextStyle(fontFamily: 'monospace')),
            ),
            const SizedBox(height: 8),
            // Live Fortschrittsanzeige für Dateiübertragung
            if (_activeProgress != null)
              Card(
                color: Colors.blue.shade50,
                elevation: 3,
                margin: const EdgeInsets.symmetric(vertical: 8),
                child: Padding(
                  padding: const EdgeInsets.all(12),
                  child: Column(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      Row(
                        mainAxisAlignment: MainAxisAlignment.spaceBetween,
                        children: [
                          Row(
                            children: [
                              Icon(
                                _activeProgress!.isFinished
                                    ? Icons.check_circle
                                    : Icons.swap_vert_circle,
                                color: _activeProgress!.isFinished ? Colors.green : Colors.blue,
                              ),
                              const SizedBox(width: 8),
                              Text(
                                _activeProgress!.isFinished
                                    ? 'Transfer abgeschlossen!'
                                    : 'Übertragung aktiv...',
                                style: const TextStyle(fontWeight: FontWeight.bold),
                              ),
                            ],
                          ),
                          Text(
                            '${_activeProgress!.progressPercent.toStringAsFixed(1)}%',
                            style: const TextStyle(
                              fontWeight: FontWeight.bold,
                              fontSize: 16,
                              color: Colors.blue,
                            ),
                          ),
                        ],
                      ),
                      const SizedBox(height: 8),
                      LinearProgressIndicator(
                        value: _activeProgress!.isFinished
                            ? 1.0
                            : (_activeProgress!.progressPercent / 100.0).clamp(0.0, 1.0),
                        minHeight: 8,
                        backgroundColor: Colors.blue.shade100,
                      ),
                      const SizedBox(height: 8),
                      Row(
                        mainAxisAlignment: MainAxisAlignment.spaceBetween,
                        children: [
                          Text(
                            '${(_activeProgress!.bytesTransferred.toDouble() / (1024 * 1024)).toStringAsFixed(2)} MB / ${(_activeProgress!.totalBytes.toDouble() / (1024 * 1024)).toStringAsFixed(2)} MB',
                            style: const TextStyle(fontSize: 12),
                          ),
                          Text(
                            '${(_activeProgress!.speedBytesPerSec.toDouble() / (1024 * 1024)).toStringAsFixed(1)} MB/s',
                            style: const TextStyle(fontSize: 12, fontWeight: FontWeight.bold, color: Colors.indigo),
                          ),
                          Text(
                            'Chunk: ${_activeProgress!.currentChunk}/${_activeProgress!.totalChunks} (ETA: ${_activeProgress!.etaSeconds}s)',
                            style: const TextStyle(fontSize: 12),
                          ),
                        ],
                      ),
                    ],
                  ),
                ),
              ),
            Row(
              children: [
                Expanded(
                  child: ElevatedButton.icon(
                    icon: const Icon(Icons.bolt),
                    style: ElevatedButton.styleFrom(backgroundColor: Colors.orange.shade100),
                    onPressed: _busy
                        ? null
                        : () async {
                            setState(() {
                              _busy = true;
                              _status = 'Sende TCP-Ping über Wi-Fi Direct...';
                            });
                            try {
                              final res = await sendTestPacket(
                                payload: 'Hallo von Rustle (${DateTime.now().second}s)!',
                              );
                              setState(() => _status = res);
                            } catch (e) {
                              setState(() => _status = 'TCP-Fehler: $e');
                            } finally {
                              setState(() => _busy = false);
                            }
                          },
                    label: const Text('3. Test-Ping (TCP :4242)'),
                  ),
                ),
                const SizedBox(width: 8),
                Expanded(
                  child: ElevatedButton.icon(
                    icon: const Icon(Icons.upload_file),
                    style: ElevatedButton.styleFrom(backgroundColor: Colors.teal.shade100),
                    onPressed: _busy ? null : _pickAndSendFile,
                    label: const Text('4. Datei senden (:4243)'),
                  ),
                ),
              ],
            ),
            const SizedBox(height: 16),
            const Text(
              'Gefundene Geräte (Antippen für automatischen BLE -> Wi-Fi Connect):',
              style: TextStyle(fontWeight: FontWeight.bold),
            ),
            const Divider(),
            Expanded(
              child: ListView.builder(
                itemCount: _peers.length,
                itemBuilder: (ctx, i) {
                  final p = _peers[i];
                  return Card(
                    child: ListTile(
                      leading: Icon(
                        p.isGo ? Icons.router : Icons.bluetooth,
                        color: p.isGo ? Colors.green : Colors.blue,
                      ),
                      title: Text('${p.name} (${p.isGo ? "GO" : "Andere"})'),
                      subtitle: Text('MAC: ${p.address} | RSSI: ${p.rssi} dBm'),
                      trailing: const Icon(Icons.wifi_tethering),
                      onTap: _busy ? null : () => _startFullHandshake(p),
                    ),
                  );
                },
              ),
            ),
          ],
        ),
      ),
    );
  }
}
