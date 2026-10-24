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
  String? _goEventBanner;
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
      _goEventBanner = null;
    });
    try {
      final res = await bleStartBroadcast(
        deviceName: isGo ? 'Host' : 'Client',
        isGo: isGo,
      );
      setState(() => _status = res);

      _pollTimer?.cancel();
      // Im Timer in _advertise(true) zusätzlich auf eingehende TCP-Pakete prüfen:
      if (isGo) {
        _pollTimer = Timer.periodic(const Duration(seconds: 1), (_) async {
          try {
            final event = await blePollAndHandleGo();
            if (event != null && mounted) {
              setState(() {
                _goEventBanner = event;
                _status = 'GO: Hotspot & TCP-Server (:4242) laufen!';
              });
            }
            final tcpMsg = await pollReceivedPackets();
            if (tcpMsg != null && mounted) {
              setState(() {
                _goEventBanner = 'TCP-PAKET EMPFANGEN:\n$tcpMsg';
                _status = 'Letztes Paket erfolgreich verarbeitet!';
              });
            }
          } catch (e) {
            if (mounted) setState(() => _status = 'GO Fehler: $e');
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
        _status = 'Scan beendet: ${found.length} Geräte. Tippe auf einen GO zum Verbinden!';
      });
    } catch (e) {
      setState(() => _status = 'Scan-Fehler: $e');
    } finally {
      setState(() => _busy = false);
    }
  }

  Future<void> _startFullHandshake(DiscoveredPeer peer) async {
    setState(() {
      _busy = true;
      _status = '1/3 Sende Paket 2 an ${peer.name}...\n2/3 Warte auf Hotspot & Paket 3...\n3/3 Verbinde Wi-Fi...';
    });
    try {
      final res = await bleHandshakeAndJoinWifi(
        targetAddress: peer.address,
        clientName: 'PeerClient',
      );
      setState(() => _status = 'ERFOLG!\n$res');
    } catch (e) {
      setState(() => _status = 'Handshake-Fehler:\n$e');
    } finally {
      setState(() => _busy = false);
    }
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(title: const Text('Rustle: BLE -> Wi-Fi Handshake')),
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
            ElevatedButton.icon(
              icon: const Icon(Icons.bolt),
              style: ElevatedButton.styleFrom(backgroundColor: Colors.orange.shade100),
              onPressed: _busy
                  ? null
                  : () async {
                setState(() {
                  _busy = true;
                  _status = 'Sende TCP-Paket über Wi-Fi Direct...';
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
              label: const Text('3. Test-Paket über Wi-Fi senden (TCP :4242)'),
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