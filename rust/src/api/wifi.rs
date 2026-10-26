use crate::platform::{
    BleDevice, DiscoveredPeer, PlatformBleManager, WifiDirectDevice, WifiDirectManager,
};
pub use crate::transfer::TransferProgress;
use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::Mutex;
use std::thread;

static WIFI_MANAGER: Mutex<Option<WifiDirectManager>> = Mutex::new(None);
static BLE_MANAGER: Mutex<Option<PlatformBleManager>> = Mutex::new(None);

// Speichert die IP des Peers (auf Client: GO-IP; auf GO: gelernte Client-IP)
static PEER_IP: Mutex<Option<String>> = Mutex::new(None);
static INCOMING_TCP_MSG: Mutex<Option<String>> = Mutex::new(None);
// Ausgehende Nachrichten-Warteschlange (wird für GO -> Client über die vom Client aufgebaute TCP-Verbindung zugestellt, falls Firewall/EACCES direktes Connect blockiert)
static OUTGOING_TCP_QUEUE: Mutex<VecDeque<String>> = Mutex::new(VecDeque::new());
static TCP_SERVER_RUNNING: Mutex<bool> = Mutex::new(false);
static IS_CURRENTLY_GO: Mutex<bool> = Mutex::new(false);
// Speichert die bereits beim GO-Start vorbereiteten Hotspot-Zugangsdaten (Pre-Warming!)
static ACTIVE_GO_CREDS: Mutex<Option<String>> = Mutex::new(None);

// File Transfer Zustände
static DOWNLOAD_DIRECTORY: Mutex<Option<String>> = Mutex::new(None);
static ACTIVE_TRANSFER_PROGRESS: Mutex<Option<TransferProgress>> = Mutex::new(None);
static TRANSFER_SERVER_RUNNING: Mutex<bool> = Mutex::new(false);

pub const RUSTLE_TCP_PORT: u16 = 4242;
pub const RUSTLE_TRANSFER_PORT: u16 = 4243;

pub fn init_core() -> Result<String, String> {
    let mut wifi = WIFI_MANAGER.lock().map_err(|e| e.to_string())?;
    *wifi = Some(WifiDirectManager::new());

    let mut ble = BLE_MANAGER.lock().map_err(|e| e.to_string())?;
    *ble = Some(PlatformBleManager::new());

    ensure_tcp_server_running();
    ensure_transfer_server_running();

    Ok("Rust Core (BLE + Wi-Fi Direct + TCP :4242/:4243) initialisiert".into())
}

/// Prüft, ob ein aktives VPN auf dem Gerät läuft (Android oder Linux)
pub fn check_active_vpn() -> Result<Option<String>, String> {
    crate::platform::check_active_vpn_platform()
}

pub fn ble_start_broadcast(device_name: String, is_go: bool) -> Result<String, String> {
    ensure_tcp_server_running();
    ensure_transfer_server_running();
    *IS_CURRENTLY_GO.lock().unwrap() = is_go;
    if is_go {
        *PEER_IP.lock().unwrap() = None;
        OUTGOING_TCP_QUEUE.lock().unwrap().clear();

        // PRE-WARMING: Starte den Wi-Fi Direct GO (Hotspot) direkt beim Aktivieren des GO-Modus
        // und hinterlege Paket 3 (SSID;PSK;IP) sofort im GATT-Server!
        let prewarmed_creds = {
            let mut wifi = WIFI_MANAGER.lock().map_err(|e| e.to_string())?;
            let w_mgr = wifi.as_mut().ok_or("Wi-Fi Manager nicht initialisiert")?;
            w_mgr.start_hotspot("DIRECT-RustleGO".into(), "password1234".into())?
        };

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

/// Auf dem GO: Prüft auf Paket 2 -> Stellt sicher, dass Paket 3 (DIRECT-...;PSK;IP) bereitsteht
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
                let c = w_mgr.start_hotspot("DIRECT-RustleGO".into(), "password1234".into())?;
                if c.starts_with("ERR:") {
                    return Err(c);
                }
                *ACTIVE_GO_CREDS.lock().unwrap() = Some(c.clone());
                let mut ble = BLE_MANAGER.lock().map_err(|e| e.to_string())?;
                if let Some(ref mut mgr) = *ble {
                    mgr.set_hotspot_credentials(&c)?
                }
                c
            }
        };

        return Ok(Some(format!(
            "Anfrage '{}' -> Wi-Fi Direct GO & TCP-Server (:4242/:4243) bereit! ({})",
            req, creds
        )));
    }

    Ok(None)
}

/// Auf dem Client: Sendet Paket 2 -> Empfängt Paket 3 (SSID;PSK;IP) -> Verbindet Wi-Fi Direct
pub fn ble_handshake_and_join_wifi(
    target_address: String,
    client_name: String,
) -> Result<String, String> {
    *IS_CURRENTLY_GO.lock().unwrap() = false;
    OUTGOING_TCP_QUEUE.lock().unwrap().clear();

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
        return Err(format!("Ungültiges Paket 3 empfangen: {}", creds));
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

    // Hintergrund-Worker für INIT_PING und Rückkanal-Polling
    let target = go_ip.clone();
    std::thread::spawn(move || {
        let mut init_done = false;
        loop {
            if *IS_CURRENTLY_GO.lock().unwrap() {
                break;
            }
            let current_peer = PEER_IP.lock().unwrap().clone();
            if current_peer.as_deref() != Some(target.as_str()) {
                break;
            }

            let probe = if !init_done { "INIT_PING" } else { "POLL" };
            if let Ok(reply) =
                crate::platform::send_tcp_packet_platform(&target, RUSTLE_TCP_PORT, probe)
            {
                init_done = true;
                if let Some(idx) = reply.find("MSG:") {
                    let msg_content = reply[idx + 4..].trim().to_string();
                    if !msg_content.is_empty() {
                        println!("[RUST DEBUG] Vom GO über Poll empfangen: {}", msg_content);
                        if let Ok(mut inbox) = INCOMING_TCP_MSG.lock() {
                            *inbox = Some(msg_content);
                        }
                    }
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(450));
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

    println!("[RUST DEBUG] Sende Testpaket an IP: {}", target_ip);
    match crate::platform::send_tcp_packet_platform(&target_ip, RUSTLE_TCP_PORT, &payload) {
        Ok(res) => Ok(res),
        Err(err) => {
            if is_go {
                println!(
                    "[RUST DEBUG] Direktes Senden vom GO blockiert ({}), stelle über Client-Rückkanal zu...",
                    err
                );
                OUTGOING_TCP_QUEUE.lock().unwrap().push_back(payload.clone());

                for _ in 0..25 {
                    std::thread::sleep(std::time::Duration::from_millis(80));
                    if OUTGOING_TCP_QUEUE.lock().unwrap().is_empty() {
                        return Ok(format!(
                            "Erfolg: Vom Client empfangen ({}) [via Rückkanal]",
                            payload
                        ));
                    }
                }
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

    let target = target_ip.clone();
    let path_clone = file_path.clone();

    std::thread::spawn(move || {
        use std::time::Duration;

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
            },
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
                let mut s = stream;
                let _ = s.set_read_timeout(Some(std::time::Duration::from_secs(5)));
                let _ = s.set_write_timeout(Some(std::time::Duration::from_secs(5)));

                let mut peer_ip_opt = None;
                if let Ok(peer_addr) = s.peer_addr() {
                    let ip_str = peer_addr.ip().to_string().replace("::ffff:", "");
                    if ip_str != "127.0.0.1" && ip_str != "::1" {
                        *PEER_IP.lock().unwrap() = Some(ip_str.clone());
                        peer_ip_opt = Some(ip_str);
                    }
                }

                let mut buf = [0u8; 4096];
                if let Ok(n) = s.read(&mut buf) {
                    if n > 0 {
                        let msg = String::from_utf8_lossy(&buf[..n]).to_string();

                        if msg == "INIT_PING" {
                            println!(
                                "[RUST DEBUG] INIT_PING von Peer {:?} empfangen",
                                peer_ip_opt
                            );
                            if let Some(ip) = peer_ip_opt {
                                if let Ok(mut inbox) = INCOMING_TCP_MSG.lock() {
                                    if inbox.is_none() {
                                        *inbox = Some(format!(
                                            "[System] Client verbunden (IP: {}) - Bereit!",
                                            ip
                                        ));
                                    }
                                }
                            }
                            let next_out = OUTGOING_TCP_QUEUE.lock().unwrap().pop_front();
                            let reply = match next_out {
                                Some(queued) => format!("MSG:{}", queued),
                                None => "ACK:INIT_OK".to_string(),
                            };
                            let _ = s.write_all(reply.as_bytes());
                            let _ = s.flush();
                        } else if msg == "POLL" {
                            let next_out = OUTGOING_TCP_QUEUE.lock().unwrap().pop_front();
                            let reply = match next_out {
                                Some(queued) => {
                                    println!(
                                        "[RUST DEBUG] Liefere ausgehende GO-Nachricht an Client aus: {}",
                                        queued
                                    );
                                    format!("MSG:{}", queued)
                                }
                                None => "ACK:EMPTY".to_string(),
                            };
                            let _ = s.write_all(reply.as_bytes());
                            let _ = s.flush();
                        } else {
                            println!("[RUST DEBUG] TCP-Nachricht empfangen: {}", msg);
                            if let Ok(mut inbox) = INCOMING_TCP_MSG.lock() {
                                *inbox = Some(msg.clone());
                            }
                            let reply = format!("ACK: Empfangen ({})", msg);
                            let _ = s.write_all(reply.as_bytes());
                            let _ = s.flush();
                        }
                    }
                }
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

                let res = crate::transfer::FileReceiver::receive_file(
                    &mut s,
                    &save_dir,
                    |progress| {
                        if let Ok(mut lock) = ACTIVE_TRANSFER_PROGRESS.lock() {
                            *lock = Some(progress.clone());
                        }
                    },
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
                        if let Ok(mut inbox) = INCOMING_TCP_MSG.lock() {
                            *inbox = Some(format!(
                                "[Datei empfangen] {}",
                                final_display_path
                            ));
                        }
                    }
                    Err(e) => {
                        println!("[RUST TRANSFER] Empfangs-Fehler: {}", e);
                    }
                }
            });
        }
    });
}
