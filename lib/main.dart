import 'dart:async';
import 'package:flutter/material.dart';
import 'package:rustle/src/rust/api/wifi.dart';
import 'package:rustle/src/rust/frb_generated.dart';
import 'package:rustle/src/rust/platform.dart';

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
  String? _incomingRequest;
  List<DiscoveredPeer> _peers = [];
  bool _busy = false;
  Timer? _pollTimer;

  @override
  void initState() {
    super.initState();
    _initCore();
  }

  @override
  void dispose() {
    _pollTimer?.cancel();
    super.dispose();
  }

  Future<void> _initCore() async {
    try {
      final res = await initCore();
      setState(() => _status = res);
    } catch (e) {
      setState(() => _status = 'Init-Fehler: $e');
    }
  }

  Future<void> _advertise(bool isGo) async {
    setState(() {
      _busy = true;
      _incomingRequest = null;
    });
    try {
      final res = await bleStartBroadcast(
        deviceName: isGo ? 'Host' : 'Client',
        isGo: isGo,
      );
      setState(() => _status = res);

      // Wenn wir GO sind, prüfen wir jede Sekunde auf eingehende Paket-2-Anfragen
      _pollTimer?.cancel();
      if (isGo) {
        _pollTimer = Timer.periodic(const Duration(seconds: 1), (_) async {
          final req = await blePollIncomingRequest();
          if (req != null && mounted) {
            setState(() {
              _incomingRequest = req;
              _status = 'PAKET 2 EMPFANGEN: "$req"';
            });
          }
        });
      }
    } catch (e) {
      setState(() => _status = 'Broadcast-Fehler: $e');
    } finally {
      setState(() => _busy = false);
    }
  }

  Future<void> _scan() async {
    setState(() {
      _busy = true;
      _status = 'Scanne 4 Sekunden lang über Rust Core...';
    });
    try {
      final found = await bleScan(durationSecs: BigInt.from(4));
      setState(() {
        _peers = found;
        _status = 'Scan beendet: ${found.length} Geräte gesehen. Tippe auf einen GO!';
      });
    } catch (e) {
      setState(() => _status = 'Scan-Fehler: $e');
    } finally {
      setState(() => _busy = false);
    }
  }

  Future<void> _sendRequestToGo(DiscoveredPeer peer) async {
    setState(() {
      _busy = true;
      _status = 'Sende Paket 2 (Verbindungs-Wunsch) an ${peer.name} (${peer.address})...';
    });
    try {
      final res = await bleSendConnectRequest(
        targetAddress: peer.address,
        clientName: 'PeerClient',
      );
      setState(() => _status = res);
    } catch (e) {
      setState(() => _status = 'Fehler bei Paket 2: $e');
    } finally {
      setState(() => _busy = false);
    }
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(title: const Text('Schritt 2: BLE Handshake (SYN)')),
      body: Padding(
        padding: const EdgeInsets.all(16),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            Row(
              children: [
                Expanded(
                  child: ElevatedButton(
                    onPressed: _busy ? null : () => _advertise(true),
                    child: const Text('1. Sende als GO'),
                  ),
                ),
                const SizedBox(width: 8),
                Expanded(
                  child: ElevatedButton(
                    onPressed: _busy ? null : _scan,
                    child: const Text('2. BLE Scanner'),
                  ),
                ),
              ],
            ),
            const SizedBox(height: 12),
            if (_incomingRequest != null)
              Container(
                padding: const EdgeInsets.all(12),
                margin: const EdgeInsets.only(bottom: 12),
                color: Colors.green.shade100,
                child: Text(
                  'Verbindungs-Wunsch erhalten: $_incomingRequest',
                  style: const TextStyle(fontWeight: FontWeight.bold, color: Colors.green),
                ),
              ),
            Container(
              padding: const EdgeInsets.all(10),
              color: Colors.black12,
              child: Text(_status, style: const TextStyle(fontFamily: 'monospace')),
            ),
            const SizedBox(height: 16),
            const Text(
              'Gefundene Geräte (Antippen, um Paket 2 zu senden):',
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
                      trailing: const Icon(Icons.send),
                      onTap: _busy ? null : () => _sendRequestToGo(p),
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