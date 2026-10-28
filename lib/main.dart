import 'dart:async';
import 'dart:io';
import 'dart:ui';
import 'package:file_picker/file_picker.dart';
import 'package:flutter/material.dart';
import 'package:path_provider/path_provider.dart';
import 'package:rustle/dev_mode_page.dart';
import 'package:rustle/src/rust/api/wifi.dart';
import 'package:rustle/src/rust/frb_generated.dart';
import 'package:rustle/src/rust/platform.dart';
import 'package:rustle/src/rust/transfer/progress.dart';
import 'package:rustle/l10n/app_localizations.dart';

Future<void> main() async {
  WidgetsFlutterBinding.ensureInitialized();
  await RustLib.init();
  runApp(const RustleApp());
}

class RustleApp extends StatefulWidget {
  const RustleApp({super.key});

  @override
  State<RustleApp> createState() => _RustleAppState();
}

class _RustleAppState extends State<RustleApp> {
  bool _isDevMode = false;

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      debugShowCheckedModeBanner: false,
      localizationsDelegates: AppLocalizations.localizationsDelegates,
      supportedLocales: AppLocalizations.supportedLocales,
      onGenerateTitle: (context) => AppLocalizations.of(context)!.appTitle,
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
            borderRadius: BorderRadius.circular(14),
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
            padding: const EdgeInsets.symmetric(horizontal: 18, vertical: 14),
            shape: RoundedRectangleBorder(borderRadius: BorderRadius.circular(12)),
          ),
        ),
      ),
      home: _isDevMode
          ? DevModePage(onExitDevMode: () => setState(() => _isDevMode = false))
          : UserHomePage(onEnterDevMode: () => setState(() => _isDevMode = true)),
    );
  }
}

class UserHomePage extends StatefulWidget {
  final VoidCallback onEnterDevMode;

  const UserHomePage({super.key, required this.onEnterDevMode});

  @override
  State<UserHomePage> createState() => _UserHomePageState();
}

class _UserHomePageState extends State<UserHomePage> {
  List<DiscoveredPeer> _peers = [];
  bool _isScanning = false;
  bool _isConnectingOrSending = false;
  bool _isReceivingReady = false;
  DiscoveredPeer? _connectedPeer;
  String? _connectedPeerAddress;
  String? _connectedPeerName;
  String Function(AppLocalizations l10n)? _statusMessageBuilder;
  String? _activeVpn;
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
    _initAndStartAutomaticScan();
  }

  @override
  void dispose() {
    _eventSub?.cancel();
    _lifecycleListener.dispose();
    super.dispose();
  }

  Future<void> _initAndStartAutomaticScan() async {
    try {
      await initCore();
      await _initDownloadDir();
      _listenToRustEvents();
      await _checkVpn();

      // Automatischer 15-Sekunden-BLE-Scan direkt nach dem Öffnen der App
      await _scanForRecipients(durationSeconds: 15);
    } catch (e) {
      if (mounted) {
        _statusMessageBuilder = (l10n) => l10n.errorInit;
      }
    }
  }

  void _listenToRustEvents() {
    _eventSub?.cancel();
    _eventSub = subscribeEvents().listen(
      (event) {
        if (!mounted) return;
        setState(() {
          if (event.vpnUpdated) {
            _activeVpn = event.activeVpn;
          }
          if (event.transferProgress != null) {
            _activeProgress = event.transferProgress;
            if (_activeProgress!.isFinished) {
              _statusMessageBuilder = (l10n) => l10n.finishedTransfer;
            }
          }
          if (event.tcpMessage != null && event.tcpMessage!.startsWith('[Datei empfangen]')) {
            _statusMessageBuilder = (_) => event.tcpMessage ?? '';
          }
        });
      },
      onError: (e) {
        debugPrint('Event-Stream Fehler: $e');
      },
    );
  }

  Future<void> _checkVpn() async {
    try {
      final vpn = await checkActiveVpn();
      if (mounted && vpn != _activeVpn) {
        setState(() => _activeVpn = vpn);
      }
    } catch (_) {}
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

  /// Scannt per BLE nach verfügbaren Empfängern (beim App-Start 15s, sowie bei Knopfdruck)
  Future<void> _scanForRecipients({int durationSeconds = 15}) async {
    if (_isScanning || _isConnectingOrSending) return;
    setState(() {
      _isScanning = true;
      _statusMessageBuilder = (l10n) => l10n.lookingForDevices;
    });

    try {
      final found = await bleScan(durationSecs: BigInt.from(durationSeconds));
      if (!mounted) return;
      // Nur echte Teilnehmer der Rustle-Anwendung anzeigen statt alle nahen BLE GEräte zu empfangen
      final rustlePeers = found.where((p) {
        return p.isGo ||
            p.name.startsWith("R_GO_") ||
            p.name.startsWith("R_CL_") ||
            p.name.contains("Rustle") ||
            p.name == "R_Peer";
      }).toList();

      setState(() {
        _peers = rustlePeers;
        _statusMessageBuilder = rustlePeers.isEmpty
            ? _statusMessageBuilder = (l10n) => l10n.noDevicesFound
            : _statusMessageBuilder = (l10n) => l10n.devicesAvailable(rustlePeers.length);
      });
      await _checkVpn();
    } catch (e) {
      if (mounted) {
        _statusMessageBuilder = (l10n) => l10n.error('Search failed');
      }
    } finally {
      if (mounted) {
        setState(() => _isScanning = false);
      }
    }
  }

  /// Startet oder beendet den Host-Modus (Wi-Fi Direct Group Owner & BLE-Advertising)
  Future<void> _toggleHosting() async {
    if (_isConnectingOrSending) return;

    if (_isReceivingReady) {
      _statusMessageBuilder = (l10n) => l10n.stopHosting;
      try {
        await bleStopBroadcast();
        if (mounted) {
          setState(() {
            _isReceivingReady = false;
            _statusMessageBuilder = (l10n) => l10n.stoppedHosting;
          });
        }
      } catch (e) {
        if (mounted) {
          _statusMessageBuilder = (l10n) => l10n.error('while cancelling');
        }
      }
    } else {
      _statusMessageBuilder = (l10n) => l10n.initBLEAndWiFi;
      try {
        final hostName = Platform.isAndroid ? 'Android' : 'Linux';
        await bleStartBroadcast(deviceName: hostName, isGo: true);
        if (mounted) {
          setState(() {
            _isReceivingReady = true;
            _statusMessageBuilder = (l10n) => l10n.hosting;
          });
        }
      } catch (e) {
        if (mounted) {
          _statusMessageBuilder = (l10n) => l10n.error(e.toString());
        }
      }
    }
  }

  /// Wird aufgerufen, sobald ein Empfänger in der Liste angetippt wird:
  /// 1. Öffnet sofort den Dateidialog
  /// 2. Verbindet sich (falls noch nicht mit diesem Peer verbunden)
  /// 3. Startet automatisch die Dateiübertragung
  Future<void> _onRecipientSelected(DiscoveredPeer peer) async {
    if (_isConnectingOrSending) return;

    // 1. Dateiauswahl-Dialog sofort öffnen
    final result = await FilePicker.platform.pickFiles();
    if (result == null || result.files.isEmpty) return;

    final filePath = result.files.single.path;
    final fileName = result.files.single.name;
    if (filePath == null) return;

    final peerCleanName = _cleanPeerName(peer.name);
    setState(() {
      _isConnectingOrSending = true;
      _activeProgress = null;
      _statusMessageBuilder = (l10n) => l10n.connectedTo(peerCleanName);
    });

    try {
      // 2. Falls noch nicht mit diesem Empfänger verbunden, Handshake & Wi-Fi Direct aufbauen
      if (_connectedPeerAddress != peer.address) {
        await bleHandshakeAndJoinWifi(
          targetAddress: peer.address,
          clientName: 'RustleUser',
        );
        _connectedPeer = peer;
        _connectedPeerAddress = peer.address;
        _connectedPeerName = peerCleanName;
        _isReceivingReady = false;
      }

      if (!mounted) return;
      setState(() {
        _statusMessageBuilder = (l10n) => l10n.sendFileTo(fileName, peerCleanName);
      });

      // 3. Dateiübertragung direkt starten
      await startFileTransfer(filePath: filePath);
    } catch (e) {
      _connectedPeer = null;
      _connectedPeerAddress = null;
      _connectedPeerName = null;
      if (mounted) {
        _statusMessageBuilder = (l10n) => l10n.error('transfer failed');
      }
    } finally {
      if (mounted) {
        setState(() => _isConnectingOrSending = false);
      }
    }
  }

  /// Sendet nach einer abgeschlossenen Übertragung oder bei bestehender Verbindung eine weitere Datei
  Future<void> _sendAnotherFile() async {
    if (_isConnectingOrSending) return;

    final result = await FilePicker.platform.pickFiles();
    if (result == null || result.files.isEmpty) return;

    final filePath = result.files.single.path;
    final fileName = result.files.single.name;
    if (filePath == null) return;

    final targetName = _connectedPeerName ?? 'verbundenen Empfänger';

    setState(() {
      _isConnectingOrSending = true;
      _activeProgress = null;
      _statusMessageBuilder = (l10n) => l10n.sendFileTo(fileName, targetName);
    });

    try {
      // Wi-Fi Direct ist bereits aktiv, direkt Übertragung starten
      await startFileTransfer(filePath: filePath);
    } catch (e) {
      if (mounted) {
        _statusMessageBuilder = (l10n) => l10n.error('transfer failed');
      }
    } finally {
      if (mounted) {
        setState(() => _isConnectingOrSending = false);
      }
    }
  }

  /// Bricht eine laufende Dateiübertragung sofort ab
  Future<void> _cancelTransfer() async {
    try {
      await cancelFileTransfer();
    } catch (e) {
      debugPrint('Fehler beim Abbrechen: $e');
    }
    if (mounted) {
      setState(() {
        _activeProgress = null;
        _isConnectingOrSending = false;
        _statusMessageBuilder = (l10n) => l10n.error('transfer cancelled');
      });
    }
  }

  String _cleanPeerName(String rawName) {
    final cleaned = rawName
        .replaceFirst(RegExp(r'^R_GO_'), '')
        .replaceFirst(RegExp(r'^R_CL_'), '')
        .trim();
    return cleaned.isEmpty ? rawName : cleaned;
  }

  String _formatEta(BigInt etaSeconds) {
    final secs = etaSeconds.toInt();
    if (secs <= 0) return '< 1s';
    if (secs < 60) return '${secs}s';
    final mins = secs ~/ 60;
    final remSecs = secs % 60;
    return '${mins}m ${remSecs}s';
  }

  @override
  Widget build(BuildContext context) {
    final l10n = AppLocalizations.of(context)!;
    return Scaffold(
      appBar: AppBar(
        title: Row(
          children: [
            const Icon(Icons.wifi_tethering, color: Color(0xFF38BDF8), size: 24),
            const SizedBox(width: 10),
            Text(
              l10n.appTitle,
              style: TextStyle(fontWeight: FontWeight.bold, letterSpacing: 0.5),
            ),
            if (_isReceivingReady) ...[
              const SizedBox(width: 10),
              Container(
                padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 3),
                decoration: BoxDecoration(
                  color: const Color(0xFF064E3B),
                  borderRadius: BorderRadius.circular(20),
                  border: Border.all(color: const Color(0xFF10B981), width: 0.8),
                ),
                child: Row(
                  mainAxisSize: MainAxisSize.min,
                  children: [
                    const Icon(Icons.circle, size: 7, color: Color(0xFF34D399)),
                    const SizedBox(width: 5),
                    Text(
                      l10n.statusHeader,
                      style: const TextStyle(
                        fontSize: 11,
                        fontWeight: FontWeight.w600,
                        color: Color(0xFF6EE7B7),
                      ),
                    ),
                  ],
                ),
              ),
            ],
          ],
        ),
        actions: [
          IconButton(
            tooltip: l10n.resetConnectionStatus,
            icon: const Icon(Icons.restore, color: Color(0xFF94A3B8)),
            onPressed: (_isConnectingOrSending || _isScanning)
                ? null
                : () async {
                    setState(() => _statusMessageBuilder = (l10n) => l10n.resetConnectionStatus);
                    try {
                      await bleStopBroadcast();
                      if (mounted) {
                        setState(() {
                          _isReceivingReady = false;
                          _peers = [];
                          _connectedPeer = null;
                          _connectedPeerAddress = null;
                          _connectedPeerName = null;
                          _activeProgress = null;
                          _statusMessageBuilder = (_) => l10n.finishedConnectionReset;
                        });
                        await _checkVpn();
                      }
                    } catch (e) {
                      if (mounted) setState(() => _statusMessageBuilder = (l10n) => l10n.error(e.toString()));
                    }
                  },
          ),
          PopupMenuButton<String>(
            icon: const Icon(Icons.settings, color: Color(0xFF94A3B8)),
            tooltip: l10n.tooltipsSettings,
            onSelected: (value) {
              if (value == 'dev_mode') {
                widget.onEnterDevMode();
              }
            },
            itemBuilder: (context) => [
              PopupMenuItem(
                value: 'dev_mode',
                child: Row(
                  children: [
                    const Icon(Icons.developer_mode, color: Color(0xFF38BDF8), size: 20),
                    const SizedBox(width: 10),
                    Text(l10n.tooltipsDevMode),
                  ],
                ),
              ),
            ],
          ),
        ],
      ),
      body: Padding(
        padding: const EdgeInsets.symmetric(horizontal: 20, vertical: 16),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            // Dezenter VPN-Hinweis, falls ein aktives VPN erkannt wurde
            if (_activeVpn != null)
              Container(
                padding: const EdgeInsets.symmetric(horizontal: 14, vertical: 10),
                margin: const EdgeInsets.only(bottom: 16),
                decoration: BoxDecoration(
                  color: const Color(0xFF451A03),
                  border: Border.all(color: const Color(0xFFB45309), width: 1),
                  borderRadius: BorderRadius.circular(12),
                ),
                child: Row(
                  children: [
                    const Icon(Icons.warning_amber_rounded, color: Color(0xFFFBBF24), size: 22),
                    const SizedBox(width: 10),
                    Expanded(
                      child: Text(
                        l10n.vpnActive(_activeVpn ?? '---') ,
                        style: const TextStyle(fontSize: 12, color: Color(0xFFFDE68A)),
                      ),
                    ),
                  ],
                ),
              ),

            // Dedizierte Steuerungs-Buttons: Scannen & Hosten nebeneinander
            Row(
              children: [
                Expanded(
                  child: SizedBox(
                    height: 52,
                    child: ElevatedButton.icon(
                      style: ElevatedButton.styleFrom(
                        backgroundColor: const Color(0xFF0284C7), // Sky 600
                        foregroundColor: Colors.white,
                        disabledBackgroundColor: const Color(0xFF1E293B),
                        disabledForegroundColor: const Color(0xFF94A3B8),
                      ),
                      onPressed: (_isScanning || _isConnectingOrSending)
                          ? null
                          : () => _scanForRecipients(durationSeconds: 15),
                      icon: _isScanning
                          ? const SizedBox(
                              width: 18,
                              height: 18,
                              child: CircularProgressIndicator(
                                strokeWidth: 2,
                                color: Color(0xFF38BDF8),
                              ),
                            )
                          : const Icon(Icons.radar, size: 20),
                      label: Text(
                        _isScanning ? l10n.scanning : l10n.scan,
                        style: const TextStyle(fontSize: 15, fontWeight: FontWeight.w600),
                      ),
                    ),
                  ),
                ),
                const SizedBox(width: 10),
                Expanded(
                  child: SizedBox(
                    height: 52,
                    child: ElevatedButton.icon(
                      style: ElevatedButton.styleFrom(
                        backgroundColor: _isReceivingReady
                            ? const Color(0xFF065F46) // Emerald 800
                            : const Color(0xFF4338CA), // Indigo 700
                        foregroundColor: _isReceivingReady
                            ? const Color(0xFF6EE7B7)
                            : Colors.white,
                        side: _isReceivingReady
                            ? const BorderSide(color: Color(0xFF10B981), width: 1.2)
                            : BorderSide.none,
                      ),
                      onPressed: _isConnectingOrSending ? null : _toggleHosting,
                      icon: Icon(
                        _isReceivingReady ? Icons.check_circle_rounded : Icons.wifi_tethering,
                        size: 20,
                        color: _isReceivingReady ? const Color(0xFF34D399) : Colors.white,
                      ),
                      label: Text(
                        _isReceivingReady ? l10n.hosting : l10n.host,
                        style: const TextStyle(fontSize: 15, fontWeight: FontWeight.w600),
                      ),
                    ),
                  ),
                ),
              ],
            ),

            if (_statusMessageBuilder != null) ...[
              const SizedBox(height: 12),
              Text(
                _statusMessageBuilder!(l10n),
                textAlign: TextAlign.center,
                style: const TextStyle(fontSize: 13, color: Color(0xFF94A3B8)),
              ),
            ],

            // Aktive Verbindung anzeigen (falls verbunden und kein Progress-Card aktiv)
            if (_connectedPeer != null && _activeProgress == null) ...[
              const SizedBox(height: 14),
              Container(
                padding: const EdgeInsets.symmetric(horizontal: 14, vertical: 10),
                decoration: BoxDecoration(
                  color: const Color(0xFF0F2942),
                  borderRadius: BorderRadius.circular(12),
                  border: Border.all(color: const Color(0xFF0284C7), width: 1),
                ),
                child: Row(
                  children: [
                    const Icon(Icons.link, color: Color(0xFF38BDF8), size: 20),
                    const SizedBox(width: 10),
                    Expanded(
                      child: Text(
                        l10n.connectedTo(_connectedPeerName ?? '---'),
                        style: const TextStyle(
                          fontWeight: FontWeight.w600,
                          fontSize: 13,
                          color: Color(0xFFE2E8F0),
                        ),
                      ),
                    ),
                    ElevatedButton.icon(
                      style: ElevatedButton.styleFrom(
                        backgroundColor: const Color(0xFF0284C7),
                        foregroundColor: Colors.white,
                        padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 8),
                        minimumSize: Size.zero,
                        tapTargetSize: MaterialTapTargetSize.shrinkWrap,
                        shape: RoundedRectangleBorder(borderRadius: BorderRadius.circular(8)),
                      ),
                      onPressed: _isConnectingOrSending ? null : _sendAnotherFile,
                      icon: const Icon(Icons.upload_file, size: 16),
                      label: Text(l10n.sendFile, style: const TextStyle(fontSize: 12)),
                    ),
                  ],
                ),
              ),
            ],

            // Vereinfachter Fortschrittsbalken (mit Abbruch & Weiteres Senden)
            if (_activeProgress != null) ...[
              const SizedBox(height: 16),
              Card(
                child: Padding(
                  padding: const EdgeInsets.all(16),
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
                                    ? Icons.check_circle_rounded
                                    : Icons.sync_rounded,
                                color: _activeProgress!.isFinished
                                    ? const Color(0xFF34D399)
                                    : const Color(0xFF38BDF8),
                                size: 20,
                              ),
                              const SizedBox(width: 8),
                              Text(
                                _activeProgress!.isFinished
                                    ? l10n.sendFile
                                    : l10n.isSending,
                                style: const TextStyle(
                                  fontWeight: FontWeight.w600,
                                  fontSize: 14,
                                ),
                              ),
                            ],
                          ),
                          Text(
                            '${_activeProgress!.progressPercent.toStringAsFixed(0)}%',
                            style: const TextStyle(
                              fontWeight: FontWeight.bold,
                              fontSize: 15,
                              color: Color(0xFF38BDF8),
                            ),
                          ),
                        ],
                      ),
                      const SizedBox(height: 12),
                      ClipRRect(
                        borderRadius: BorderRadius.circular(6),
                        child: LinearProgressIndicator(
                          value: _activeProgress!.isFinished
                              ? 1.0
                              : (_activeProgress!.progressPercent / 100.0).clamp(0.0, 1.0),
                          minHeight: 8,
                          backgroundColor: const Color(0xFF334155),
                          valueColor: AlwaysStoppedAnimation<Color>(
                            _activeProgress!.isFinished
                                ? const Color(0xFF34D399)
                                : const Color(0xFF38BDF8),
                          ),
                        ),
                      ),
                      const SizedBox(height: 12),
                      Row(
                        mainAxisAlignment: MainAxisAlignment.spaceBetween,
                        children: [
                          Row(
                            children: [
                              const Icon(Icons.speed, size: 16, color: Color(0xFF818CF8)),
                              const SizedBox(width: 5),
                              Text(
                                '${(_activeProgress!.speedBytesPerSec.toDouble() / (1024 * 1024)).toStringAsFixed(1)} MB/s',
                                style: const TextStyle(
                                  fontSize: 13,
                                  fontWeight: FontWeight.bold,
                                  color: Color(0xFF818CF8),
                                ),
                              ),
                            ],
                          ),
                          Row(
                            children: [
                              const Icon(Icons.schedule, size: 15, color: Color(0xFF94A3B8)),
                              const SizedBox(width: 5),
                              Text(
                                _activeProgress!.isFinished
                                    ? l10n.finished
                                    : l10n.remaining(_formatEta(_activeProgress!.etaSeconds)),
                                style: const TextStyle(
                                  fontSize: 12.5,
                                  color: Color(0xFF94A3B8),
                                ),
                              ),
                            ],
                          ),
                        ],
                      ),

                      // Knöpfe für Übertragungsabbruch und nachfolgendes Senden
                      const SizedBox(height: 14),
                      if (!_activeProgress!.isFinished) ...[
                        // Während der Übertragung: Abbruch-Button
                        SizedBox(
                          width: double.infinity,
                          child: OutlinedButton.icon(
                            style: OutlinedButton.styleFrom(
                              foregroundColor: const Color(0xFFF87171),
                              side: const BorderSide(color: Color(0xFFEF4444), width: 1.2),
                              padding: const EdgeInsets.symmetric(vertical: 10),
                              shape: RoundedRectangleBorder(
                                borderRadius: BorderRadius.circular(10),
                              ),
                            ),
                            onPressed: _cancelTransfer,
                            icon: const Icon(Icons.close_rounded, size: 18),
                            label: Text(
                              l10n.cancelTransfer,
                              style: TextStyle(fontWeight: FontWeight.w600, fontSize: 13.5),
                            ),
                          ),
                        ),
                      ] else ...[
                        // Nach der Übertragung: "Weitere Datei senden" und "Schließen"
                        Row(
                          children: [
                            Expanded(
                              flex: 3,
                              child: ElevatedButton.icon(
                                style: ElevatedButton.styleFrom(
                                  backgroundColor: const Color(0xFF0284C7),
                                  foregroundColor: Colors.white,
                                  padding: const EdgeInsets.symmetric(vertical: 10),
                                  shape: RoundedRectangleBorder(
                                    borderRadius: BorderRadius.circular(10),
                                  ),
                                ),
                                onPressed: (_isConnectingOrSending || _connectedPeer == null)
                                    ? null
                                    : _sendAnotherFile,
                                icon: const Icon(Icons.add_to_photos_rounded, size: 18),
                                label: Text(
                                  l10n.sendAnotherFile,
                                  style: TextStyle(fontWeight: FontWeight.w600, fontSize: 13.5),
                                ),
                              ),
                            ),
                            const SizedBox(width: 8),
                            Expanded(
                              flex: 2,
                              child: OutlinedButton.icon(
                                style: OutlinedButton.styleFrom(
                                  foregroundColor: const Color(0xFF94A3B8),
                                  side: const BorderSide(color: Color(0xFF475569)),
                                  padding: const EdgeInsets.symmetric(vertical: 10),
                                  shape: RoundedRectangleBorder(
                                    borderRadius: BorderRadius.circular(10),
                                  ),
                                ),
                                onPressed: () {
                                  setState(() => _activeProgress = null);
                                },
                                icon: const Icon(Icons.done_all_rounded, size: 17),
                                label: Text(
                                 l10n.finished,
                                  style: TextStyle(fontSize: 13.5),
                                ),
                              ),
                            ),
                          ],
                        ),
                      ],
                    ],
                  ),
                ),
              ),
            ],

            const SizedBox(height: 20),
            Text(
              l10n.availableDevices,
              style: TextStyle(
                fontWeight: FontWeight.bold,
                fontSize: 14,
                color: Color(0xFFCBD5E1),
              ),
            ),
            const SizedBox(height: 8),

            // Liste der gefundenen Empfänger
            Expanded(
              child: _peers.isEmpty
                  ? Center(
                      child: Column(
                        mainAxisSize: MainAxisSize.min,
                        children: [
                          Icon(
                            _isScanning ? Icons.radar : Icons.devices_other,
                            size: 46,
                            color: const Color(0xFF475569),
                          ),
                          const SizedBox(height: 12),
                          Text(
                            _isScanning
                                ? l10n.lookingForDevices
                                : l10n.noDevicesFound,
                            textAlign: TextAlign.center,
                            style: const TextStyle(color: Color(0xFF64748B), fontSize: 13.5),
                          ),
                        ],
                      ),
                    )
                  : ListView.builder(
                      itemCount: _peers.length,
                      itemBuilder: (ctx, i) {
                        final peer = _peers[i];
                        final isConnected = _connectedPeerAddress == peer.address;
                        return Card(
                          margin: const EdgeInsets.only(bottom: 10),
                          child: ListTile(
                            contentPadding: const EdgeInsets.symmetric(
                              horizontal: 16,
                              vertical: 6,
                            ),
                            leading: Container(
                              padding: const EdgeInsets.all(10),
                              decoration: BoxDecoration(
                                color: isConnected
                                    ? const Color(0xFF064E3B)
                                    : const Color(0xFF1E3A8A),
                                borderRadius: BorderRadius.circular(10),
                              ),
                              child: Icon(
                                Icons.devices,
                                color: isConnected
                                    ? const Color(0xFF34D399)
                                    : const Color(0xFF60A5FA),
                                size: 22,
                              ),
                            ),
                            title: Text(
                              _cleanPeerName(peer.name),
                              style: const TextStyle(
                                fontWeight: FontWeight.w600,
                                fontSize: 15,
                              ),
                            ),
                            subtitle: Text(
                              isConnected
                                  ? l10n.connected
                                  : l10n.readyToSendFile,
                              style: const TextStyle(
                                color: Color(0xFF94A3B8),
                                fontSize: 12,
                              ),
                            ),
                            trailing: const Icon(
                              Icons.send_rounded,
                              color: Color(0xFF38BDF8),
                              size: 20,
                            ),
                            onTap: _isConnectingOrSending
                                ? null
                                : () => _onRecipientSelected(peer),
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
