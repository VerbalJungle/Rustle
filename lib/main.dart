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
  runApp(const RustleApp());
}

class RustleApp extends StatelessWidget {
  const RustleApp({super.key});

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      debugShowCheckedModeBanner: false,
      title: 'Rustle',
      themeMode: ThemeMode.dark,
      darkTheme: ThemeData(
        useMaterial3: true,
        brightness: Brightness.dark,
        scaffoldBackgroundColor: const Color(0xFF0F172A), // Slate 900
        colorScheme: const ColorScheme.dark(
          primary: Color(0xFF38BDF8), // Sky 400
          secondary: Color(0xFF818CF8), // Indigo 400
          surface: Color(0xFF1E293B), // Slate 800
          error: Color(0xFFF87171),
        ),
        cardTheme: CardThemeData(
          color: const Color(0xFF1E293B),
          elevation: 0,
          shape: RoundedRectangleBorder(
            borderRadius: BorderRadius.circular(12),
            side: const BorderSide(color: Color(0xFF334155), width: 1),
          ),
        ),
        appBarTheme: const AppBarTheme(
          backgroundColor: Color(0xFF0F172A),
          foregroundColor: Colors.white,
          elevation: 0,
          scrolledUnderElevation: 0,
        ),
        elevatedButtonTheme: ElevatedButtonThemeData(
          style: ElevatedButton.styleFrom(
            elevation: 0,
            padding: const EdgeInsets.symmetric(horizontal: 14, vertical: 12),
            shape: RoundedRectangleBorder(borderRadius: BorderRadius.circular(10)),
          ),
        ),
      ),
      home: const BleDiagnosticPage(),
    );
  }
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
  String? _wifiChannelInfo;
  List<DiscoveredPeer> _peers = [];
  bool _busy = false;
  StreamSubscription<RustEvent>? _eventSub;
  TransferProgress? _activeProgress;
  late final AppLifecycleListener _lifecycleListener;

  @override
  void initState() {
    super.initState();
    _lifecycleListener = AppLifecycleListener(
      onExitRequested: () async {
        await _eventSub?.cancel();
        try {
          await bleStopBroadcast();
        } catch (_) {}
        return AppExitResponse.exit;
      },
      onDetach: () {
        _eventSub?.cancel();
        bleStopBroadcast();
      },
    );
    _initCore();
  }

  @override
  void dispose() {
    _eventSub?.cancel();
    _lifecycleListener.dispose();
    bleStopBroadcast();
    super.dispose();
  }

  Future<void> _initCore() async {
    try {
      final res = await initCore();
      setState(() => _status = res);
      await _initDownloadDir();
      _listenToRustEvents();
      await _checkVpn();
      await _refreshWifiChannelInfo();
    } catch (e) {
      setState(() => _status = 'Init-Fehler: $e');
    }
  }

  void _listenToRustEvents() {
    _eventSub?.cancel();
    _eventSub = subscribeEvents().listen(
      (event) {
        if (!mounted) return;
        bool shouldRefreshChannel = false;
        setState(() {
          if (event.vpnUpdated) {
            _activeVpn = event.activeVpn;
          }
          if (event.wifiChannelUpdated) {
            _wifiChannelInfo = event.wifiChannelInfo;
          }
          if (event.goHandshake != null && _goEventBanner == null) {
            _goEventBanner = event.goHandshake;
            _status = 'GO: Hotspot & TCP-Server (:4242/:4243) laufen!';
            shouldRefreshChannel = true;
          }
          if (event.tcpMessage != null) {
            _goEventBanner = 'NACHRICHT EMPFANGEN:\n${event.tcpMessage}';
            _status = 'Letztes Paket erfolgreich verarbeitet!';
          }
          if (event.transferProgress != null) {
            if (_wifiChannelInfo == null) {
              shouldRefreshChannel = true;
            }
            _activeProgress = event.transferProgress;
          }
        });
        if (shouldRefreshChannel) {
          _refreshWifiChannelInfo();
        }
      },
      onError: (e) {
        if (mounted) {
          setState(() => _status = 'Stream-Fehler: $e');
        }
      },
    );
  }

  Future<void> _refreshWifiChannelInfo() async {
    try {
      final info = await getActiveWifiChannelInfo();
      if (mounted && info != _wifiChannelInfo) {
        setState(() => _wifiChannelInfo = info);
      }
    } catch (e) {
      debugPrint('Wi-Fi channel check error: $e');
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
      await _refreshWifiChannelInfo();
    } catch (e) {
      setState(() => _status = 'Broadcast-Fehler: $e');
    } finally {
      setState(() => _busy = false);
    }
  }

  Future<void> _resetConnection() async {
    setState(() {
      _busy = true;
      _goEventBanner = null;
      _activeProgress = null;
      _wifiChannelInfo = null;
      _status = 'Setze Hotspot & BLE zurück (stelle Normal-WLAN wieder her)...';
    });
    try {
      final res = await bleStopBroadcast();
      setState(() {
        _peers = [];
        _status = res;
      });
      await _checkVpn();
      await _refreshWifiChannelInfo();
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
      _status = '1/3 Lese Zugangsdaten von ${peer.name}...\n2/3 Verbinde Wi-Fi Direct...';
    });
    try {
      final res = await bleHandshakeAndJoinWifi(
        targetAddress: peer.address,
        clientName: 'PeerClient',
      );
      setState(() => _status = 'ERFOLG!\n$res');
      await _refreshWifiChannelInfo();
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

    await _refreshWifiChannelInfo();

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
        title: Row(
          children: const [
            Icon(Icons.wifi_tethering, color: Color(0xFF38BDF8), size: 24),
            SizedBox(width: 10),
            Text('Rustle', style: TextStyle(fontWeight: FontWeight.bold, letterSpacing: 0.5)),
          ],
        ),
        actions: [
          IconButton(
            tooltip: 'Hotspot & BLE zurücksetzen (Normal-WLAN wiederherstellen)',
            icon: const Icon(Icons.restore, color: Color(0xFF94A3B8)),
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
                padding: const EdgeInsets.symmetric(horizontal: 14, vertical: 12),
                margin: const EdgeInsets.only(bottom: 12),
                decoration: BoxDecoration(
                  color: const Color(0xFF451A03),
                  border: Border.all(color: const Color(0xFFB45309), width: 1.2),
                  borderRadius: BorderRadius.circular(10),
                ),
                child: Row(
                  children: [
                    const Icon(Icons.warning_amber_rounded, color: Color(0xFFFBBF24), size: 26),
                    const SizedBox(width: 12),
                    Expanded(
                      child: Column(
                        crossAxisAlignment: CrossAxisAlignment.start,
                        children: [
                          Text(
                            'Aktives VPN erkannt ($_activeVpn)',
                            style: const TextStyle(
                              fontWeight: FontWeight.bold,
                              color: Color(0xFFFDE68A),
                              fontSize: 13,
                            ),
                          ),
                          const SizedBox(height: 2),
                          const Text(
                            'Ein VPN kann den lokalen Wi-Fi Direct Datenverkehr stören. '
                            'Bei Verbindungsproblemen bitte das VPN vorübergehend pausieren.',
                            style: TextStyle(fontSize: 11.5, color: Color(0xFFFEF3C7)),
                          ),
                        ],
                      ),
                    ),
                  ],
                ),
              ),

            // Obere Steuerungs-Buttons
            Row(
              children: [
                Expanded(
                  child: ElevatedButton.icon(
                    style: ElevatedButton.styleFrom(
                      backgroundColor: const Color(0xFF0284C7), // Sky 600
                      foregroundColor: Colors.white,
                    ),
                    onPressed: _busy ? null : () => _advertise(true),
                    icon: const Icon(Icons.router, size: 18),
                    label: const Text('1. GO starten'),
                  ),
                ),
                const SizedBox(width: 8),
                Expanded(
                  child: ElevatedButton.icon(
                    style: ElevatedButton.styleFrom(
                      backgroundColor: const Color(0xFF4F46E5), // Indigo 600
                      foregroundColor: Colors.white,
                    ),
                    onPressed: _busy ? null : _scan,
                    icon: const Icon(Icons.search, size: 18),
                    label: const Text('2. Scannen'),
                  ),
                ),
                const SizedBox(width: 8),
                ElevatedButton(
                  style: ElevatedButton.styleFrom(
                    backgroundColor: const Color(0xFF7F1D1D),
                    foregroundColor: const Color(0xFFFCA5A5),
                    padding: const EdgeInsets.symmetric(horizontal: 12),
                  ),
                  onPressed: _busy ? null : _resetConnection,
                  child: const Icon(Icons.power_settings_new, size: 18),
                ),
              ],
            ),
            const SizedBox(height: 12),

            // GO Event Banner (Dark Green)
            if (_goEventBanner != null)
              Container(
                padding: const EdgeInsets.all(12),
                margin: const EdgeInsets.only(bottom: 12),
                decoration: BoxDecoration(
                  color: const Color(0xFF064E3B), // Deep Emerald
                  border: Border.all(color: const Color(0xFF059669), width: 1.2),
                  borderRadius: BorderRadius.circular(10),
                ),
                child: Text(
                  _goEventBanner!,
                  style: const TextStyle(
                    fontWeight: FontWeight.w600,
                    color: Color(0xFF6EE7B7),
                    fontSize: 13,
                  ),
                ),
              ),

            // Status Terminal Box
            Container(
              padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 10),
              decoration: BoxDecoration(
                color: const Color(0xFF020617), // Fast Schwarz Slate 950
                border: Border.all(color: const Color(0xFF334155), width: 1),
                borderRadius: BorderRadius.circular(8),
              ),
              child: Row(
                children: [
                  const Icon(Icons.terminal, color: Color(0xFF64748B), size: 18),
                  const SizedBox(width: 8),
                  Expanded(
                    child: Text(
                      _status,
                      style: const TextStyle(
                        fontFamily: 'monospace',
                        fontSize: 12,
                        color: Color(0xFF94A3B8),
                      ),
                    ),
                  ),
                ],
              ),
            ),
            const SizedBox(height: 8),

            // Live Fortschrittsanzeige für Dateiübertragung (Dark Mode)
            if (_activeProgress != null)
              Card(
                margin: const EdgeInsets.symmetric(vertical: 8),
                child: Padding(
                  padding: const EdgeInsets.all(14),
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
                                color: _activeProgress!.isFinished
                                    ? const Color(0xFF34D399)
                                    : const Color(0xFF38BDF8),
                              ),
                              const SizedBox(width: 8),
                              Text(
                                _activeProgress!.isFinished
                                    ? 'Transfer abgeschlossen!'
                                    : 'Übertragung aktiv...',
                                style: const TextStyle(fontWeight: FontWeight.bold, fontSize: 14),
                              ),
                            ],
                          ),
                          Text(
                            '${_activeProgress!.progressPercent.toStringAsFixed(1)}%',
                            style: const TextStyle(
                              fontWeight: FontWeight.bold,
                              fontSize: 16,
                              color: Color(0xFF38BDF8),
                            ),
                          ),
                        ],
                      ),
                      const SizedBox(height: 10),
                      ClipRRect(
                        borderRadius: BorderRadius.circular(4),
                        child: LinearProgressIndicator(
                          value: _activeProgress!.isFinished
                              ? 1.0
                              : (_activeProgress!.progressPercent / 100.0).clamp(0.0, 1.0),
                          minHeight: 6,
                          backgroundColor: const Color(0xFF334155),
                          valueColor: const AlwaysStoppedAnimation<Color>(Color(0xFF38BDF8)),
                        ),
                      ),
                      const SizedBox(height: 10),
                      Row(
                        mainAxisAlignment: MainAxisAlignment.spaceBetween,
                        children: [
                          Text(
                            '${(_activeProgress!.bytesTransferred.toDouble() / (1024 * 1024)).toStringAsFixed(2)} MB / ${(_activeProgress!.totalBytes.toDouble() / (1024 * 1024)).toStringAsFixed(2)} MB',
                            style: const TextStyle(fontSize: 11.5, color: Color(0xFF94A3B8)),
                          ),
                          Row(
                            mainAxisSize: MainAxisSize.min,
                            children: [
                              Text(
                                '${(_activeProgress!.speedBytesPerSec.toDouble() / (1024 * 1024)).toStringAsFixed(1)} MB/s',
                                style: const TextStyle(
                                  fontSize: 12,
                                  fontWeight: FontWeight.bold,
                                  color: Color(0xFF818CF8),
                                ),
                              ),
                              if (_wifiChannelInfo != null && _wifiChannelInfo!.isNotEmpty) ...[
                                const SizedBox(width: 6),
                                Container(
                                  padding: const EdgeInsets.symmetric(horizontal: 6, vertical: 2),
                                  decoration: BoxDecoration(
                                    color: _wifiChannelInfo!.startsWith('5')
                                        ? const Color(0xFF064E3B)
                                        : const Color(0xFF1E293B),
                                    borderRadius: BorderRadius.circular(5),
                                    border: Border.all(
                                      color: _wifiChannelInfo!.startsWith('5')
                                          ? const Color(0xFF10B981)
                                          : const Color(0xFF475569),
                                      width: 0.8,
                                    ),
                                  ),
                                  child: Text(
                                    _wifiChannelInfo!,
                                    style: TextStyle(
                                      fontSize: 10.5,
                                      fontWeight: FontWeight.w600,
                                      color: _wifiChannelInfo!.startsWith('5')
                                          ? const Color(0xFF6EE7B7)
                                          : const Color(0xFFCBD5E1),
                                    ),
                                  ),
                                ),
                              ],
                            ],
                          ),
                          Text(
                            'Chunk: ${_activeProgress!.currentChunk}/${_activeProgress!.totalChunks}',
                            style: const TextStyle(fontSize: 11.5, color: Color(0xFF94A3B8)),
                          ),
                        ],
                      ),
                    ],
                  ),
                ),
              ),

            // Aktions-Buttons: Ping & Datei senden
            Row(
              children: [
                Expanded(
                  child: ElevatedButton.icon(
                    icon: const Icon(Icons.bolt, size: 18),
                    style: ElevatedButton.styleFrom(
                      backgroundColor: const Color(0xFF92400E), // Warmes Bernstein/Amber
                      foregroundColor: const Color(0xFFFDE68A),
                    ),
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
                    label: const Text('3. TCP-Ping (:4242)'),
                  ),
                ),
                const SizedBox(width: 8),
                Expanded(
                  child: ElevatedButton.icon(
                    icon: const Icon(Icons.upload_file, size: 18),
                    style: ElevatedButton.styleFrom(
                      backgroundColor: const Color(0xFF0F766E), // Teal 700
                      foregroundColor: const Color(0xFFCCFBF1),
                    ),
                    onPressed: _busy ? null : _pickAndSendFile,
                    label: const Text('4. Datei senden (:4243)'),
                  ),
                ),
              ],
            ),
            const SizedBox(height: 16),
            const Text(
              'Gefundene Geräte (Tippen zum Verbinden):',
              style: TextStyle(fontWeight: FontWeight.bold, fontSize: 13, color: Color(0xFFCBD5E1)),
            ),
            const SizedBox(height: 6),
            Expanded(
              child: _peers.isEmpty
                  ? Center(
                      child: Column(
                        mainAxisSize: MainAxisSize.min,
                        children: const [
                          Icon(Icons.bluetooth_searching, size: 40, color: Color(0xFF475569)),
                          SizedBox(height: 8),
                          Text(
                            'Keine Geräte gefunden.\nTippe auf "2. Scannen", um nach Peers zu suchen.',
                            textAlign: TextAlign.center,
                            style: TextStyle(color: Color(0xFF64748B), fontSize: 13),
                          ),
                        ],
                      ),
                    )
                  : ListView.builder(
                      itemCount: _peers.length,
                      itemBuilder: (ctx, i) {
                        final p = _peers[i];
                        return Card(
                          margin: const EdgeInsets.only(bottom: 8),
                          child: ListTile(
                            leading: Container(
                              padding: const EdgeInsets.all(8),
                              decoration: BoxDecoration(
                                color: p.isGo
                                    ? const Color(0xFF064E3B)
                                    : const Color(0xFF1E3A8A),
                                borderRadius: BorderRadius.circular(8),
                              ),
                              child: Icon(
                                p.isGo ? Icons.router : Icons.bluetooth,
                                color: p.isGo
                                    ? const Color(0xFF34D399)
                                    : const Color(0xFF60A5FA),
                                size: 20,
                              ),
                            ),
                            title: Text(
                              '${p.name} ${p.isGo ? "(GO)" : ""}',
                              style: const TextStyle(fontWeight: FontWeight.w600, fontSize: 14),
                            ),
                            subtitle: Text(
                              'MAC: ${p.address} | RSSI: ${p.rssi} dBm',
                              style: const TextStyle(color: Color(0xFF94A3B8), fontSize: 11.5),
                            ),
                            trailing: const Icon(Icons.wifi_tethering, color: Color(0xFF38BDF8), size: 20),
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
