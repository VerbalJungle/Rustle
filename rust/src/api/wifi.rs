use crate::frb_generated::StreamSink;
use crate::platform::{
    BleDevice, DiscoveredPeer, PlatformBleManager, WifiDirectDevice, WifiDirectManager,
};
pub use crate::transfer::TransferProgress;
use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::thread;
use std::time::Duration;

/// Ereignis-Paket, das vom Rust-Backend per StreamSink direkt an Flutter gepusht wird.
#[derive(Debug, Clone)]
pub struct RustEvent {
    pub go_handshake: Option<String>,
    pub tcp_message: Option<String>,
    pub transfer_progress: Option<TransferProgress>,
    pub vpn_updated: bool,
    pub active_vpn: Option<String>,
    pub wifi_channel_updated: bool,
    pub wifi_channel_info: Option<String>,
}

impl RustEvent {
    fn from_go_handshake(msg: String) -> Self {
        Self {
            go_handshake: Some(msg),
            tcp_message: None,
            transfer_progress: None,
            vpn_updated: false,
            active_vpn: None,
            wifi_channel_updated: false,
            wifi_channel_info: None,
        }
    }

    fn from_tcp_message(msg: String) -> Self {
        Self {
            go_handshake: None,
            tcp_message: Some(msg),
            transfer_progress: None,
            vpn_updated: false,
            active_vpn: None,
            wifi_channel_updated: false,
            wifi_channel_info: None,
        }
    }

    fn from_transfer_progress(progress: TransferProgress) -> Self {
        Self {
            go_handshake: None,
            tcp_message: None,
            transfer_progress: Some(progress),
            vpn_updated: false,
            active_vpn: None,
            wifi_channel_updated: false,
            wifi_channel_info: None,
        }
    }

    fn from_vpn_status(vpn: Option<String>) -> Self {
        Self {
            go_handshake: None,
            tcp_message: None,
            transfer_progress: None,
            vpn_updated: true,
            active_vpn: vpn,
            wifi_channel_updated: false,
            wifi_channel_info: None,
        }
    }

    fn from_wifi_channel_info(info: Option<String>) -> Self {
        Self {
            go_handshake: None,
            tcp_message: None,
            transfer_progress: None,
            vpn_updated: false,
            active_vpn: None,
            wifi_channel_updated: true,
            wifi_channel_info: info,
        }
    }
}

static EVENT_SINK: Mutex<Option<StreamSink<RustEvent>>> = Mutex::new(None);
static MONITOR_RUNNING: Mutex<bool> = Mutex::new(false);

static WIFI_MANAGER: Mutex<Option<WifiDirectManager>> = Mutex::new(None);
static BLE_MANAGER: Mutex<Option<PlatformBleManager>> = Mutex::new(None);

// Speichert die IP des Peers (auf Client: GO-IP; auf GO: gelernte Client-IP)
static PEER_IP: Mutex<Option<String>> = Mutex::new(None);
static INCOMING_TCP_MSG: Mutex<Option<String>> = Mutex::new(None);
// Persistenter, bidirektionaler TCP-Stream für den Kontroll- und Nachrichtenkanal (:4242)
static ACTIVE_CONTROL_STREAM: Mutex<Option<TcpStream>> = Mutex::new(None);
// Ausgehende Nachrichten-Warteschlange (wird direkt beim Aufbau des persistenten Streams zugestellt, falls eine Nachricht vor dem Handshake gesendet wurde)
static OUTGOING_TCP_QUEUE: Mutex<VecDeque<String>> = Mutex::new(VecDeque::new());
static TCP_SERVER_RUNNING: Mutex<bool> = Mutex::new(false);
static IS_CURRENTLY_GO: Mutex<bool> = Mutex::new(false);
// Speichert die bereits beim GO-Start vorbereiteten Hotspot-Zugangsdaten (Pre-Warming)
static ACTIVE_GO_CREDS: Mutex<Option<String>> = Mutex::new(None);

// File Transfer Zustände
static DOWNLOAD_DIRECTORY: Mutex<Option<String>> = Mutex::new(None);
static ACTIVE_TRANSFER_PROGRESS: Mutex<Option<TransferProgress>> = Mutex::new(None);
static TRANSFER_SERVER_RUNNING: Mutex<bool> = Mutex::new(false);
static TRANSFER_CANCELLED: AtomicBool = AtomicBool::new(false);

pub const RUSTLE_TCP_PORT: u16 = 4242;
pub const RUSTLE_TRANSFER_PORT: u16 = 4243;

fn emit_event(event: RustEvent) {
    if let Ok(lock) = EVENT_SINK.lock() {
        if let Some(ref sink) = *lock {
            let _ = sink.add(event);
        }
    }
}

fn close_active_control_stream() {
    if let Ok(mut lock) = ACTIVE_CONTROL_STREAM.lock() {
        if let Some(stream) = lock.take() {
            let _ = stream.shutdown(Shutdown::Both);
        }
    }
}

fn write_line_to_stream(stream: &mut TcpStream, line: &str) -> Result<(), String> {
    let clean = line.replace('\r', " ").replace('\n', " ");
    let framed = format!("{}\n", clean);
    stream
        .write_all(framed.as_bytes())
        .map_err(|e| format!("Fehler beim Schreiben auf TCP-Stream: {}", e))?;
    stream
        .flush()
        .map_err(|e| format!("Fehler beim Flushen des TCP-Streams: {}", e))?;
    Ok(())
}

/// Registriert einen verbundenen Kontroll-Stream (:4242) als kontinuierliche, bidirektionale Leitung
/// und verarbeitet eingehende Zeilen in Echtzeit, bis die Verbindung getrennt wird.
fn handle_persistent_control_stream(stream: TcpStream, peer_ip_opt: Option<String>) {
    let _ = stream.set_nodelay(true);
    let _ = stream.set_read_timeout(None); // Blockierendes Lesen für kontinuierlichen Stream
    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));

    let write_clone = match stream.try_clone() {
        Ok(c) => c,
        Err(e) => {
            println!("[RUST TCP] Konnte Stream nicht klonen: {}", e);
            return;
        }
    };

    // Speichere die Schreib-Hälfte global und spüle ggf. wartende Nachrichten sofort heraus
    {
        if let Ok(mut lock) = ACTIVE_CONTROL_STREAM.lock() {
            if let Some(old) = lock.take() {
                let _ = old.shutdown(Shutdown::Both);
            }
            *lock = Some(write_clone);
        }

        let mut queued_msgs = Vec::new();
        if let Ok(mut q) = OUTGOING_TCP_QUEUE.lock() {
            while let Some(msg) = q.pop_front() {
                queued_msgs.push(msg);
            }
        }

        if !queued_msgs.is_empty() {
            if let Ok(mut lock) = ACTIVE_CONTROL_STREAM.lock() {
                if let Some(ref mut active_s) = *lock {
                    for msg in queued_msgs {
                        let _ = write_line_to_stream(active_s, &format!("MSG:{}", msg));
                    }
                }
            }
        }
    }

    let mut reader = BufReader::new(stream);
    let mut line = String::new();

    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => {
                println!(
                    "[RUST TCP] Kontinuierlicher Stream zu {:?} wurde vom Peer geschlossen (EOF).",
                    peer_ip_opt
                );
                break;
            }
            Ok(_) => {
                let msg = line.trim();
                if msg.is_empty() {
                    continue;
                }

                if msg == "INIT_PING" {
                    println!(
                        "[RUST DEBUG] INIT_PING über kontinuierlichen Stream von {:?} empfangen",
                        peer_ip_opt
                    );
                    if let Some(ref ip) = peer_ip_opt {
                        let sys_msg = format!("[System] Client verbunden (IP: {}) - Bereit!", ip);
                        if let Ok(mut inbox) = INCOMING_TCP_MSG.lock() {
                            if inbox.is_none() {
                                *inbox = Some(sys_msg.clone());
                            }
                        }
                        emit_event(RustEvent::from_tcp_message(sys_msg));
                    }
                    if let Ok(mut lock) = ACTIVE_CONTROL_STREAM.lock() {
                        if let Some(ref mut s) = *lock {
                            let _ = write_line_to_stream(s, "ACK:INIT_OK");
                        }
                    }
                } else if msg == "ACK:INIT_OK" || msg == "POLL" || msg == "ACK:EMPTY" {
                    // Handshake-Bestätigung oder Legacy-Keepalive -> nichts im UI anzeigen
                } else if let Some(ack_rest) = msg.strip_prefix("ACK:") {
                    println!("[RUST DEBUG] ACK vom Peer erhalten: {}", ack_rest.trim());
                } else {
                    let content = msg.strip_prefix("MSG:").unwrap_or(msg).trim().to_string();
                    if !content.is_empty() {
                        println!(
                            "[RUST DEBUG] Nachricht über kontinuierlichen Stream empfangen: {}",
                            content
                        );
                        if let Ok(mut inbox) = INCOMING_TCP_MSG.lock() {
                            *inbox = Some(content.clone());
                        }
                        emit_event(RustEvent::from_tcp_message(content.clone()));

                        if let Ok(mut lock) = ACTIVE_CONTROL_STREAM.lock() {
                            if let Some(ref mut s) = *lock {
                                let _ =
                                    write_line_to_stream(s, &format!("ACK: Empfangen ({})", content));
                            }
                        }
                    }
                }
            }
            Err(e) => {
                println!(
                    "[RUST TCP] Lesefehler auf kontinuierlichem Stream ({:?}): {}",
                    peer_ip_opt, e
                );
                break;
            }
        }
    }
}

/// Registriert einen StreamSink für Echtzeit-Events vom Rust-Core an Flutter.
pub fn subscribe_events(sink: StreamSink<RustEvent>) -> Result<(), String> {
    if let Ok(mut lock) = EVENT_SINK.lock() {
        *lock = Some(sink);
    }

    // Initialen VPN-Status und WLAN-Kanal direkt pushen
    if let Ok(vpn) = check_active_vpn() {
        emit_event(RustEvent::from_vpn_status(vpn));
    }
    if let Ok(ch_info) = get_active_wifi_channel_info() {
        emit_event(RustEvent::from_wifi_channel_info(ch_info));
    }

    ensure_background_monitor_running();
    Ok(())
}

fn ensure_background_monitor_running() {
    let mut running = MONITOR_RUNNING.lock().unwrap();
    if *running {
        return;
    }
    *running = true;

    thread::spawn(|| {
        let mut tick: u64 = 0;
        let mut last_vpn: Option<String> = None;
        let mut last_channel_info: Option<String> = None;

        loop {
            thread::sleep(Duration::from_millis(250));
            tick = tick.wrapping_add(1);

            // Alle ~2 Sekunden VPN-Status und WLAN-Frequenzband/Kanal prüfen und bei Änderung pushen
            if tick % 8 == 0 {
                if let Ok(vpn) = check_active_vpn() {
                    if vpn != last_vpn {
                        last_vpn = vpn.clone();
                        emit_event(RustEvent::from_vpn_status(vpn));
                    }
                }
                if let Ok(ch_info) = get_active_wifi_channel_info() {
                    if ch_info != last_channel_info {
                        last_channel_info = ch_info.clone();
                        emit_event(RustEvent::from_wifi_channel_info(ch_info));
                    }
                }
            }

            // Falls wir GO sind, auf eingehende GATT-Handshakes prüfen (Fallback für Nicht-Connectionless)
            let is_go = *IS_CURRENTLY_GO.lock().unwrap();
            if is_go {
                if let Ok(Some(event)) = ble_poll_and_handle_go() {
                    emit_event(RustEvent::from_go_handshake(event));
                }
            }
        }
    });
}

pub fn init_core() -> Result<String, String> {
    let mut wifi = WIFI_MANAGER.lock().map_err(|e| e.to_string())?;
    *wifi = Some(WifiDirectManager::new());

    let mut ble = BLE_MANAGER.lock().map_err(|e| e.to_string())?;
    *ble = Some(PlatformBleManager::new());

    ensure_tcp_server_running();
    ensure_transfer_server_running();
    ensure_background_monitor_running();

    Ok("Rust Core (BLE + Wi-Fi Direct + TCP :4242/:4243) initialisiert".into())
}

/// Prüft, ob ein aktives VPN auf dem Gerät läuft (Android oder Linux)
pub fn check_active_vpn() -> Result<Option<String>, String> {
    crate::platform::check_active_vpn_platform()
}

/// Liest das aktuell genutzte WLAN-Frequenzband und den Kanal aus (z. B. "5 GHz • Ch 36")
pub fn get_active_wifi_channel_info() -> Result<Option<String>, String> {
    crate::platform::get_active_wifi_channel_info_platform()
}

pub fn ble_start_broadcast(device_name: String, is_go: bool) -> Result<String, String> {
    ensure_tcp_server_running();
    ensure_transfer_server_running();
    close_active_control_stream();
    *IS_CURRENTLY_GO.lock().unwrap() = is_go;
    if is_go {
        *PEER_IP.lock().unwrap() = None;
        OUTGOING_TCP_QUEUE.lock().unwrap().clear();

        // PRE-WARMING: Startet den Wi-Fi Direct GO (Hotspot) direkt beim Aktivieren des GO-Modus
        // und hinterlegt die Zugangsdaten (SSID;PSK;IP) im GATT-Server bzw. BLE Advertising
        let prewarmed_creds = {
            let mut wifi = WIFI_MANAGER.lock().map_err(|e| e.to_string())?;
            let w_mgr = wifi.as_mut().ok_or("Wi-Fi Manager nicht initialisiert")?;
            w_mgr.start_hotspot("DIRECT-RustleGO".into(), "rustle42".into())?
        };

        if let Ok(ch_info) = get_active_wifi_channel_info() {
            emit_event(RustEvent::from_wifi_channel_info(ch_info));
        }

        if !prewarmed_creds.starts_with("ERR:") {
            *ACTIVE_GO_CREDS.lock().unwrap() = Some(prewarmed_creds.clone());
            let mut ble = BLE_MANAGER.lock().map_err(|e| e.to_string())?;
            if let Some(ref mut mgr) = *ble {
                let _ = mgr.set_hotspot_credentials(&prewarmed_creds);
                let adv_msg = mgr.start_advertising(&device_name, is_go)?;
                return Ok(format!("{}\nWi-Fi Direct GO bereit: {}", adv_msg, prewarmed_creds));
            }
        }
    }

    let mut ble = BLE_MANAGER.lock().map_err(|e| e.to_string())?;
    if let Some(ref mut mgr) = *ble {
        mgr.start_advertising(&device_name, is_go)
    } else {
        Err("BLE Manager nicht initialisiert".into())
    }
}

/// Beendet BLE Advertising/GATT sowie den Wi-Fi Direct Hotspot / Client-Connect
/// und stellt (insbesondere unter Linux) das vorherige normale WLAN wieder her.
pub fn ble_stop_broadcast() -> Result<String, String> {
    *IS_CURRENTLY_GO.lock().unwrap() = false;
    *PEER_IP.lock().unwrap() = None;
    *ACTIVE_GO_CREDS.lock().unwrap() = None;
    OUTGOING_TCP_QUEUE.lock().unwrap().clear();
    close_active_control_stream();
    TRANSFER_CANCELLED.store(true, Ordering::SeqCst);
    if let Ok(mut p) = ACTIVE_TRANSFER_PROGRESS.lock() {
        *p = None;
    }

    let ble_msg = {
        let mut ble = BLE_MANAGER.lock().map_err(|e| e.to_string())?;
        if let Some(ref mut mgr) = *ble {
            mgr.stop_advertising().unwrap_or_else(|e| e)
        } else {
            "BLE inaktiv".into()
        }
    };

    let wifi_msg = {
        let mut wifi = WIFI_MANAGER.lock().map_err(|e| e.to_string())?;
        if let Some(ref mut w_mgr) = *wifi {
            w_mgr.stop_hotspot().unwrap_or_else(|e| e)
        } else {
            "Wi-Fi inaktiv".into()
        }
    };

    emit_event(RustEvent::from_wifi_channel_info(None));

    Ok(format!("Reset durchgeführt: {} | {}", ble_msg, wifi_msg))
}

pub fn ble_scan(duration_secs: u64) -> Result<Vec<DiscoveredPeer>, String> {
    let mut ble = BLE_MANAGER.lock().map_err(|e| e.to_string())?;
    if let Some(ref mut mgr) = *ble {
        mgr.scan_peers(duration_secs)
    } else {
        Err("BLE Manager nicht initialisiert".into())
    }
}

/// Auf dem GO: Prüft auf eingehende Anfragen -> Stellt sicher, dass Zugangsdaten bereitstehen
pub fn ble_poll_and_handle_go() -> Result<Option<String>, String> {
    let incoming = {
        let mut ble = BLE_MANAGER.lock().map_err(|e| e.to_string())?;
        let mgr = ble.as_mut().ok_or("BLE Manager nicht initialisiert")?;
        mgr.poll_incoming_request()?
    };

    if let Some(req) = incoming {
        *IS_CURRENTLY_GO.lock().unwrap() = true;
        ensure_tcp_server_running();
        ensure_transfer_server_running();

        let existing_creds = ACTIVE_GO_CREDS.lock().unwrap().clone();
        let creds = match existing_creds {
            Some(c) if !c.starts_with("ERR:") => c,
            _ => {
                let mut wifi = WIFI_MANAGER.lock().map_err(|e| e.to_string())?;
                let w_mgr = wifi.as_mut().ok_or("Wi-Fi Manager nicht initialisiert")?;
                let c = w_mgr.start_hotspot("DIRECT-RustleGO".into(), "rustle42".into())?;
                if c.starts_with("ERR:") {
                    return Err(c);
                }
                *ACTIVE_GO_CREDS.lock().unwrap() = Some(c.clone());
                let mut ble = BLE_MANAGER.lock().map_err(|e| e.to_string())?;
                if let Some(ref mut mgr) = *ble {
                    mgr.set_hotspot_credentials(&c)?;
                }
                c
            }
        };

        if let Ok(ch_info) = get_active_wifi_channel_info() {
            emit_event(RustEvent::from_wifi_channel_info(ch_info));
        }

        return Ok(Some(format!(
            "Anfrage '{}' -> Wi-Fi Direct GO & TCP-Server (:4242/:4243) bereit! ({})",
            req, creds
        )));
    }

    Ok(None)
}

/// Auf dem Client: Sendet Anfrage / holt Zugangsdaten (Connectionless BLE Handshake) -> Verbindet Wi-Fi Direct
pub fn ble_handshake_and_join_wifi(
    target_address: String,
    client_name: String,
) -> Result<String, String> {
    *IS_CURRENTLY_GO.lock().unwrap() = false;
    OUTGOING_TCP_QUEUE.lock().unwrap().clear();
    close_active_control_stream();

    let creds = {
        let mut ble = BLE_MANAGER.lock().map_err(|e| e.to_string())?;
        let mgr = ble.as_mut().ok_or("BLE Manager nicht initialisiert")?;
        mgr.send_connection_request(&target_address, &client_name)?
    };

    if creds.starts_with("ERR:") {
        return Err(creds);
    }

    let parts: Vec<&str> = creds.split(';').collect();
    if parts.len() < 2 {
        return Err(format!("Ungültige Zugangsdaten empfangen: {}", creds));
    }

    let ssid = parts[0].trim().to_string();
    let password = parts[1].trim().to_string();
    let go_ip = if parts.len() >= 3 && !parts[2].trim().is_empty() {
        parts[2].trim().to_string()
    } else {
        "10.42.0.1".to_string()
    };

    println!("[RUST DEBUG] Erhalte GO-IP: {}", go_ip);
    *PEER_IP.lock().unwrap() = Some(go_ip.clone());

    ensure_tcp_server_running();
    ensure_transfer_server_running();

    let wifi_res = {
        let mut wifi = WIFI_MANAGER.lock().map_err(|e| e.to_string())?;
        let w_mgr = wifi.as_mut().ok_or("Wi-Fi Manager nicht initialisiert")?;
        w_mgr.connect_to_hotspot(ssid.clone(), password)?
    };

    if wifi_res.starts_with("ERR:") {
        return Err(wifi_res);
    }

    if let Ok(ch_info) = get_active_wifi_channel_info() {
        emit_event(RustEvent::from_wifi_channel_info(ch_info));
    }

    // Hintergrund-Worker für den kontinuierlichen, bidirektionalen Kontroll-Stream (:4242)
    // Öffnet den Socket nur EINMALig (mit automatischem Reconnect bei Verbindungsabbruch)
    // statt alle 450ms einen neuen Socket zu erzeugen!
    let target = go_ip.clone();
    std::thread::spawn(move || {
        loop {
            if *IS_CURRENTLY_GO.lock().unwrap() {
                break;
            }
            let current_peer = PEER_IP.lock().unwrap().clone();
            if current_peer.as_deref() != Some(target.as_str()) {
                break;
            }

            match crate::platform::create_connected_wifi_stream(
                &target,
                RUSTLE_TCP_PORT,
                Duration::from_secs(4),
            ) {
                Ok(mut stream) => {
                    println!(
                        "[RUST TCP] Kontinuierlicher Kontroll-Stream zu {}:{} hergestellt!",
                        target, RUSTLE_TCP_PORT
                    );
                    if let Err(e) = write_line_to_stream(&mut stream, "INIT_PING") {
                        println!("[RUST TCP] Fehler beim Senden von INIT_PING: {}", e);
                        std::thread::sleep(Duration::from_millis(500));
                        continue;
                    }
                    // Blockiert und liest kontinuierlich auf demselben Socket, bis die Verbindung endet
                    handle_persistent_control_stream(stream, Some(target.clone()));
                }
                Err(e) => {
                    println!(
                        "[RUST TCP] Warte auf Kontroll-Stream zu {}:{} ({})...",
                        target, RUSTLE_TCP_PORT, e
                    );
                }
            }

            std::thread::sleep(Duration::from_millis(800));
        }
    });

    Ok(format!(
        "Verbunden mit '{}' (GO-IP: {}:{}) -> Bereit!",
        ssid, go_ip, RUSTLE_TCP_PORT
    ))
}

pub fn send_test_packet(payload: String) -> Result<String, String> {
    let target_ip = {
        let lock = PEER_IP.lock().unwrap();
        match &*lock {
            Some(ip) => ip.clone(),
            None => {
                return Err(
                    "ERR: Ziel-IP des Peers noch nicht bekannt! Warte auf Verbindung vom Client."
                        .into(),
                )
            }
        }
    };

    let is_go = *IS_CURRENTLY_GO.lock().unwrap();

    // 1. Primärer Pfad: Direkt über den bereits offenen, kontinuierlichen TCP-Stream senden (kein neuer Socket!)
    let mut sent_over_persistent = false;
    if let Ok(mut lock) = ACTIVE_CONTROL_STREAM.lock() {
        if let Some(ref mut stream) = *lock {
            match write_line_to_stream(stream, &format!("MSG:{}", payload)) {
                Ok(()) => {
                    sent_over_persistent = true;
                }
                Err(e) => {
                    println!(
                        "[RUST TCP] Persistenter Stream nicht mehr beschreibbar ({}), baue neu auf...",
                        e
                    );
                    *lock = None;
                }
            }
        }
    }

    if sent_over_persistent {
        return Ok(format!(
            "Über kontinuierlichen Stream an {} gesendet: {}",
            target_ip, payload
        ));
    }

    // 2. Falls noch kein persistenter Stream offen ist: Jetzt einmalig öffnen und als kontinuierlichen Stream behalten
    println!(
        "[RUST DEBUG] Öffne neuen kontinuierlichen Stream zu {}:{}...",
        target_ip, RUSTLE_TCP_PORT
    );
    match crate::platform::create_connected_wifi_stream(
        &target_ip,
        RUSTLE_TCP_PORT,
        Duration::from_secs(3),
    ) {
        Ok(mut stream) => {
            write_line_to_stream(&mut stream, &format!("MSG:{}", payload))?;
            let ip_clone = target_ip.clone();
            std::thread::spawn(move || {
                handle_persistent_control_stream(stream, Some(ip_clone));
            });
            Ok(format!(
                "Kontinuierlicher Stream geöffnet & gesendet an {}: {}",
                target_ip, payload
            ))
        }
        Err(err) => {
            if is_go {
                println!(
                    "[RUST DEBUG] Direktes Öffnen vom GO nicht möglich ({}), reihe für Client-Stream ein...",
                    err
                );
                OUTGOING_TCP_QUEUE.lock().unwrap().push_back(payload.clone());
                Ok(format!(
                    "In Warteschlange für Client ({}) bereitgestellt!",
                    target_ip
                ))
            } else {
                Err(err)
            }
        }
    }
}

pub fn poll_received_packets() -> Result<Option<String>, String> {
    let mut lock = INCOMING_TCP_MSG.lock().map_err(|e| e.to_string())?;
    Ok(lock.take())
}

// -------------------------------------------------------------
// High-Speed Dateiübertragung (Chunking über TCP :4243)
// -------------------------------------------------------------

/// Setzt das Speicherverzeichnis für eingehende Dateien (z. B. Downloads)
pub fn set_download_directory(dir_path: String) -> Result<(), String> {
    let mut lock = DOWNLOAD_DIRECTORY.lock().map_err(|e| e.to_string())?;
    *lock = Some(dir_path);
    Ok(())
}

/// Fragt den aktuellen Fortschritt der aktiven Dateiübertragung ab
pub fn poll_transfer_progress() -> Result<Option<TransferProgress>, String> {
    let lock = ACTIVE_TRANSFER_PROGRESS.lock().map_err(|e| e.to_string())?;
    Ok(lock.clone())
}

/// Bricht eine laufende Dateiübertragung (Senden oder Empfangen) sofort ab
pub fn cancel_file_transfer() -> Result<(), String> {
    TRANSFER_CANCELLED.store(true, Ordering::SeqCst);
    if let Ok(mut lock) = ACTIVE_TRANSFER_PROGRESS.lock() {
        *lock = None;
    }
    println!("[RUST TRANSFER] Übertragung wurde vom Nutzer abgebrochen");
    Ok(())
}

/// Startet das Streaming einer Datei an den verbundenen Peer im Hintergrund
pub fn start_file_transfer(file_path: String) -> Result<String, String> {
    let target_ip = {
        let lock = PEER_IP.lock().unwrap();
        match &*lock {
            Some(ip) => ip.clone(),
            None => return Err("ERR: Ziel-IP noch nicht bekannt! Bitte erst verbinden.".into()),
        }
    };

    let p = std::path::Path::new(&file_path);
    if !p.exists() {
        return Err(format!("Datei '{}' existiert nicht!", file_path));
    }

    ensure_transfer_server_running();

    TRANSFER_CANCELLED.store(false, Ordering::SeqCst);
    let target = target_ip.clone();
    let path_clone = file_path.clone();

    std::thread::spawn(move || {
        println!(
            "[RUST TRANSFER] Verbinde zu {}:{} für Datei '{}' (VPN-Bypass aktiv)...",
            target, RUSTLE_TRANSFER_PORT, path_clone
        );

        let mut stream = match crate::platform::create_connected_wifi_stream(
            &target,
            RUSTLE_TRANSFER_PORT,
            Duration::from_secs(6),
        ) {
            Ok(s) => s,
            Err(e) => {
                println!(
                    "[RUST TRANSFER] Connect-Fehler zu {}:{}: {}",
                    target, RUSTLE_TRANSFER_PORT, e
                );
                if let Ok(mut lock) = ACTIVE_TRANSFER_PROGRESS.lock() {
                    *lock = None;
                }
                return;
            }
        };

        if let Ok(ch_info) = get_active_wifi_channel_info() {
            emit_event(RustEvent::from_wifi_channel_info(ch_info));
        }

        let file_id = format!(
            "file_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
        );

        let res = crate::transfer::FileSender::send_file(
            &mut stream,
            &path_clone,
            file_id,
            |progress| {
                if let Ok(mut lock) = ACTIVE_TRANSFER_PROGRESS.lock() {
                    *lock = Some(progress.clone());
                }
                emit_event(RustEvent::from_transfer_progress(progress.clone()));
            },
            || TRANSFER_CANCELLED.load(Ordering::SeqCst),
        );

        match res {
            Ok(bytes) => {
                println!("[RUST TRANSFER] Datei erfolgreich gesendet ({} Bytes)", bytes);
            }
            Err(err) => {
                println!("[RUST TRANSFER] Fehler beim Senden: {}", err);
            }
        }
    });

    Ok(format!("Sende '{}' an {}:{}", file_path, target_ip, RUSTLE_TRANSFER_PORT))
}

fn ensure_tcp_server_running() {
    let mut running = TCP_SERVER_RUNNING.lock().unwrap();
    if *running {
        return;
    }
    *running = true;

    thread::spawn(|| {
        let listener = match TcpListener::bind(("0.0.0.0", RUSTLE_TCP_PORT)) {
            Ok(l) => l,
            Err(e) => {
                println!("[RUST DEBUG] TCP Bind Fehler: {}", e);
                if let Ok(mut flag) = TCP_SERVER_RUNNING.lock() {
                    *flag = false;
                }
                return;
            }
        };

        println!("[RUST DEBUG] TCP-Server lauscht auf 0.0.0.0:{}", RUSTLE_TCP_PORT);

        for stream in listener.incoming().flatten() {
            thread::spawn(move || {
                let mut peer_ip_opt = None;
                if let Ok(peer_addr) = stream.peer_addr() {
                    let ip_str = peer_addr.ip().to_string().replace("::ffff:", "");
                    if ip_str != "127.0.0.1" && ip_str != "::1" {
                        *PEER_IP.lock().unwrap() = Some(ip_str.clone());
                        peer_ip_opt = Some(ip_str);
                    }
                }

                println!(
                    "[RUST TCP] Eingehende persistente Kontroll-Verbindung von {:?}",
                    peer_ip_opt
                );
                handle_persistent_control_stream(stream, peer_ip_opt);
            });
        }
    });
}

fn ensure_transfer_server_running() {
    let mut running = TRANSFER_SERVER_RUNNING.lock().unwrap();
    if *running {
        return;
    }
    *running = true;

    thread::spawn(|| {
        let listener = match TcpListener::bind(("0.0.0.0", RUSTLE_TRANSFER_PORT)) {
            Ok(l) => l,
            Err(e) => {
                println!("[RUST TRANSFER] Transfer Bind Fehler: {}", e);
                if let Ok(mut flag) = TRANSFER_SERVER_RUNNING.lock() {
                    *flag = false;
                }
                return;
            }
        };

        println!(
            "[RUST TRANSFER] File-Transfer-Server lauscht auf 0.0.0.0:{}",
            RUSTLE_TRANSFER_PORT
        );

        for stream in listener.incoming().flatten() {
            thread::spawn(move || {
                let mut s = stream;
                let save_dir = {
                    let lock = DOWNLOAD_DIRECTORY.lock().unwrap();
                    lock.clone().unwrap_or_else(|| {
                        std::env::temp_dir().to_string_lossy().to_string()
                    })
                };

                println!(
                    "[RUST TRANSFER] Eingehende Dateiübertragung! Zielverzeichnis: {}",
                    save_dir
                );

                if let Ok(ch_info) = get_active_wifi_channel_info() {
                    emit_event(RustEvent::from_wifi_channel_info(ch_info));
                }

                TRANSFER_CANCELLED.store(false, Ordering::SeqCst);
                let res = crate::transfer::FileReceiver::receive_file(
                    &mut s,
                    &save_dir,
                    |progress| {
                        if let Ok(mut lock) = ACTIVE_TRANSFER_PROGRESS.lock() {
                            *lock = Some(progress.clone());
                        }
                        emit_event(RustEvent::from_transfer_progress(progress.clone()));
                    },
                    || TRANSFER_CANCELLED.load(Ordering::SeqCst),
                );

                match res {
                    Ok(path) => {
                        let final_display_path =
                            match crate::platform::export_file_to_downloads_platform(&path) {
                                Ok(exported) => exported,
                                Err(err) => {
                                    println!(
                                        "[RUST TRANSFER] Hinweis beim MediaStore-Export: {}",
                                        err
                                    );
                                    path.display().to_string()
                                }
                            };

                        println!(
                            "[RUST TRANSFER] Datei erfolgreich gespeichert: {}",
                            final_display_path
                        );
                        let received_msg = format!("[Datei empfangen] {}", final_display_path);
                        if let Ok(mut inbox) = INCOMING_TCP_MSG.lock() {
                            *inbox = Some(received_msg.clone());
                        }
                        emit_event(RustEvent::from_tcp_message(received_msg));
                    }
                    Err(e) => {
                        println!("[RUST TRANSFER] Empfangs-Fehler: {}", e);
                    }
                }
            });
        }
    });
}
