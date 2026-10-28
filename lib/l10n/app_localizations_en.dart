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
  String get resetConnectionStatus => 'Reset WiFi and BLE';

  @override
  String get finishedConnectionReset =>
      'Connection reseted, old WiFi active again';

  @override
  String get error => 'Error:';

  @override
  String vpnActive(String vpnName) {
    return 'VPN $vpnName active, if you encounter any problems please try to deactivate it';
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
  String connectedTo(String peerName) {
    return 'Connected to: $peerName';
  }

  @override
  String get sendFile => 'Send file:';

  @override
  String get isSending => 'Sending file';

  @override
  String get finished => 'Finished';

  @override
  String remaining(String seconds) {
    return 'Remaining: $seconds';
  }

  @override
  String get abortTransfer => 'Abort transfer';

  @override
  String get sendAnotherFile => 'Send another file';

  @override
  String get availableDevices => 'Available devices';

  @override
  String get lookingForDevices => 'Looking for nearby devices...';

  @override
  String get noDevicesFound => 'No devices found';

  @override
  String get connected => 'Connected';

  @override
  String get readyToSendFile => 'Ready to send files';

  @override
  String get tooltipsSettings => 'Settings';

  @override
  String get tooltipsDevMode => 'Switch to Dev Mode';

  @override
  String get tooltipReset => 'Reset Connections';
}
