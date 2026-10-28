// ignore: unused_import
import 'package:intl/intl.dart' as intl;

import 'app_localizations.dart';

// ignore_for_file: type=lint

/// The translations for English (`en`).
class AppLocalizationsEn extends AppLocalizations {
  AppLocalizationsEn([String locale = 'en']) : super(locale);

  @override
  String get appTitle => 'Rustle';

  @override
  String get statusHeader => 'Ready to receive';

  @override
  String get initBLEAndWiFi => 'Initialise BLE and WiFi';

  @override
  String get resetConnectionStatus => 'Reset WiFi and BLE';

  @override
  String get finishedConnectionReset =>
      'Connection reset, reconnected to previous WiFi';

  @override
  String error(String e) {
    return 'Error: $e';
  }

  @override
  String get errorInit => 'Initialisation error';

  @override
  String vpnActive(String vpnName) {
    return 'VPN $vpnName is active. If you encounter any issues, try to disable it';
  }

  @override
  String get scan => 'Scan';

  @override
  String get scanning => 'Scanning';

  @override
  String get host => 'Host';

  @override
  String get hosting => 'Currently hosting';

  @override
  String get stopHosting => 'Stop hosting';

  @override
  String get stoppedHosting => 'Stopped hosting';

  @override
  String connectedTo(String peerName) {
    return 'Connected to: $peerName';
  }

  @override
  String get sendFile => 'Send file:';

  @override
  String get isSending => 'Sending file';

  @override
  String sendFileTo(String filename, String peername) {
    return 'Sending $filename to $peername';
  }

  @override
  String get finished => 'Finished';

  @override
  String get finishedTransfer => 'Finished transfer';

  @override
  String remaining(String seconds) {
    return 'Remaining: $seconds';
  }

  @override
  String get cancelTransfer => 'Cancel transfer';

  @override
  String get sendAnotherFile => 'Send another file';

  @override
  String get availableDevices => 'Available devices';

  @override
  String devicesAvailable(int number) {
    return '$number Devices available';
  }

  @override
  String get lookingForDevices => 'Looking for nearby devices...';

  @override
  String get noDevicesFound => 'No devices found';

  @override
  String get connected => 'Connected';

  @override
  String get readyToSendFile => 'Ready to send file';

  @override
  String get tooltipsSettings => 'Settings';

  @override
  String get tooltipsDevMode => 'Switch to Dev Mode';

  @override
  String get tooltipReset => 'Reset Connections';

  @override
  String blank(String b) {
    return '$b';
  }
}
