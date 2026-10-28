import 'dart:async';

import 'package:flutter/foundation.dart';
import 'package:flutter/widgets.dart';
import 'package:flutter_localizations/flutter_localizations.dart';
import 'package:intl/intl.dart' as intl;

import 'app_localizations_de.dart';
import 'app_localizations_en.dart';

// ignore_for_file: type=lint

/// Callers can lookup localized strings with an instance of AppLocalizations
/// returned by `AppLocalizations.of(context)`.
///
/// Applications need to include `AppLocalizations.delegate()` in their app's
/// `localizationDelegates` list, and the locales they support in the app's
/// `supportedLocales` list. For example:
///
/// ```dart
/// import 'l10n/app_localizations.dart';
///
/// return MaterialApp(
///   localizationsDelegates: AppLocalizations.localizationsDelegates,
///   supportedLocales: AppLocalizations.supportedLocales,
///   home: MyApplicationHome(),
/// );
/// ```
///
/// ## Update pubspec.yaml
///
/// Please make sure to update your pubspec.yaml to include the following
/// packages:
///
/// ```yaml
/// dependencies:
///   # Internationalization support.
///   flutter_localizations:
///     sdk: flutter
///   intl: any # Use the pinned version from flutter_localizations
///
///   # Rest of dependencies
/// ```
///
/// ## iOS Applications
///
/// iOS applications define key application metadata, including supported
/// locales, in an Info.plist file that is built into the application bundle.
/// To configure the locales supported by your app, you’ll need to edit this
/// file.
///
/// First, open your project’s ios/Runner.xcworkspace Xcode workspace file.
/// Then, in the Project Navigator, open the Info.plist file under the Runner
/// project’s Runner folder.
///
/// Next, select the Information Property List item, select Add Item from the
/// Editor menu, then select Localizations from the pop-up menu.
///
/// Select and expand the newly-created Localizations item then, for each
/// locale your application supports, add a new item and select the locale
/// you wish to add from the pop-up menu in the Value field. This list should
/// be consistent with the languages listed in the AppLocalizations.supportedLocales
/// property.
abstract class AppLocalizations {
  AppLocalizations(String locale)
    : localeName = intl.Intl.canonicalizedLocale(locale.toString());

  final String localeName;

  static AppLocalizations? of(BuildContext context) {
    return Localizations.of<AppLocalizations>(context, AppLocalizations);
  }

  static const LocalizationsDelegate<AppLocalizations> delegate =
      _AppLocalizationsDelegate();

  /// A list of this localizations delegate along with the default localizations
  /// delegates.
  ///
  /// Returns a list of localizations delegates containing this delegate along with
  /// GlobalMaterialLocalizations.delegate, GlobalCupertinoLocalizations.delegate,
  /// and GlobalWidgetsLocalizations.delegate.
  ///
  /// Additional delegates can be added by appending to this list in
  /// MaterialApp. This list does not have to be used at all if a custom list
  /// of delegates is preferred or required.
  static const List<LocalizationsDelegate<dynamic>> localizationsDelegates =
      <LocalizationsDelegate<dynamic>>[
        delegate,
        GlobalMaterialLocalizations.delegate,
        GlobalCupertinoLocalizations.delegate,
        GlobalWidgetsLocalizations.delegate,
      ];

  /// A list of this localizations delegate's supported locales.
  static const List<Locale> supportedLocales = <Locale>[
    Locale('de'),
    Locale('en'),
  ];

  /// No description provided for @appTitle.
  ///
  /// In en, this message translates to:
  /// **'Rustle'**
  String get appTitle;

  /// No description provided for @statusHeader.
  ///
  /// In en, this message translates to:
  /// **'Ready to receive'**
  String get statusHeader;

  /// No description provided for @initBLEAndWiFi.
  ///
  /// In en, this message translates to:
  /// **'Initialise BLE and WiFi'**
  String get initBLEAndWiFi;

  /// No description provided for @resetConnectionStatus.
  ///
  /// In en, this message translates to:
  /// **'Reset WiFi and BLE'**
  String get resetConnectionStatus;

  /// No description provided for @finishedConnectionReset.
  ///
  /// In en, this message translates to:
  /// **'Connection reset, reconnected to previous WiFi'**
  String get finishedConnectionReset;

  /// No description provided for @error.
  ///
  /// In en, this message translates to:
  /// **'Error: {e}'**
  String error(String e);

  /// No description provided for @errorInit.
  ///
  /// In en, this message translates to:
  /// **'Initialisation error'**
  String get errorInit;

  /// No description provided for @vpnActive.
  ///
  /// In en, this message translates to:
  /// **'VPN {vpnName} active, if you encounter any problems please try to deactivate it'**
  String vpnActive(String vpnName);

  /// No description provided for @scan.
  ///
  /// In en, this message translates to:
  /// **'Scan'**
  String get scan;

  /// No description provided for @scanning.
  ///
  /// In en, this message translates to:
  /// **'Scanning'**
  String get scanning;

  /// No description provided for @host.
  ///
  /// In en, this message translates to:
  /// **'Host'**
  String get host;

  /// No description provided for @hosting.
  ///
  /// In en, this message translates to:
  /// **'Currently hosting'**
  String get hosting;

  /// No description provided for @stopHosting.
  ///
  /// In en, this message translates to:
  /// **'Stop hosting'**
  String get stopHosting;

  /// No description provided for @stoppedHosting.
  ///
  /// In en, this message translates to:
  /// **'Stopped hosting'**
  String get stoppedHosting;

  /// No description provided for @connectedTo.
  ///
  /// In en, this message translates to:
  /// **'Connected to: {peerName}'**
  String connectedTo(String peerName);

  /// No description provided for @sendFile.
  ///
  /// In en, this message translates to:
  /// **'Send file:'**
  String get sendFile;

  /// No description provided for @isSending.
  ///
  /// In en, this message translates to:
  /// **'Sending file'**
  String get isSending;

  /// No description provided for @sendFileTo.
  ///
  /// In en, this message translates to:
  /// **'Sending {filename} to {peername}'**
  String sendFileTo(String filename, String peername);

  /// No description provided for @finished.
  ///
  /// In en, this message translates to:
  /// **'Finished'**
  String get finished;

  /// No description provided for @finishedTransfer.
  ///
  /// In en, this message translates to:
  /// **'Finished transfer'**
  String get finishedTransfer;

  /// No description provided for @remaining.
  ///
  /// In en, this message translates to:
  /// **'Remaining: {seconds}'**
  String remaining(String seconds);

  /// No description provided for @cancelTransfer.
  ///
  /// In en, this message translates to:
  /// **'Cancel transfer'**
  String get cancelTransfer;

  /// No description provided for @sendAnotherFile.
  ///
  /// In en, this message translates to:
  /// **'Send another file'**
  String get sendAnotherFile;

  /// No description provided for @availableDevices.
  ///
  /// In en, this message translates to:
  /// **'Available devices'**
  String get availableDevices;

  /// No description provided for @devicesAvailable.
  ///
  /// In en, this message translates to:
  /// **'{number} Devices available'**
  String devicesAvailable(int number);

  /// No description provided for @lookingForDevices.
  ///
  /// In en, this message translates to:
  /// **'Looking for nearby devices...'**
  String get lookingForDevices;

  /// No description provided for @noDevicesFound.
  ///
  /// In en, this message translates to:
  /// **'No devices found'**
  String get noDevicesFound;

  /// No description provided for @connected.
  ///
  /// In en, this message translates to:
  /// **'Connected'**
  String get connected;

  /// No description provided for @readyToSendFile.
  ///
  /// In en, this message translates to:
  /// **'Ready to send files'**
  String get readyToSendFile;

  /// No description provided for @tooltipsSettings.
  ///
  /// In en, this message translates to:
  /// **'Settings'**
  String get tooltipsSettings;

  /// No description provided for @tooltipsDevMode.
  ///
  /// In en, this message translates to:
  /// **'Switch to Dev Mode'**
  String get tooltipsDevMode;

  /// No description provided for @tooltipReset.
  ///
  /// In en, this message translates to:
  /// **'Reset Connections'**
  String get tooltipReset;

  /// No description provided for @blank.
  ///
  /// In en, this message translates to:
  /// **'{b}'**
  String blank(String b);
}

class _AppLocalizationsDelegate
    extends LocalizationsDelegate<AppLocalizations> {
  const _AppLocalizationsDelegate();

  @override
  Future<AppLocalizations> load(Locale locale) {
    return SynchronousFuture<AppLocalizations>(lookupAppLocalizations(locale));
  }

  @override
  bool isSupported(Locale locale) =>
      <String>['de', 'en'].contains(locale.languageCode);

  @override
  bool shouldReload(_AppLocalizationsDelegate old) => false;
}

AppLocalizations lookupAppLocalizations(Locale locale) {
  // Lookup logic when only language code is specified.
  switch (locale.languageCode) {
    case 'de':
      return AppLocalizationsDe();
    case 'en':
      return AppLocalizationsEn();
  }

  throw FlutterError(
    'AppLocalizations.delegate failed to load unsupported locale "$locale". This is likely '
    'an issue with the localizations generation tool. Please file an issue '
    'on GitHub with a reproducible sample app and the gen-l10n configuration '
    'that was used.',
  );
}
