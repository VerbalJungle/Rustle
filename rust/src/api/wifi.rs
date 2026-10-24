use crate::platform::{
    BleDevice, DiscoveredPeer, PlatformBleManager, WifiDirectDevice, WifiDirectManager,
};
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
// Ausgehende Nachrichten-Warteschlange (wird für GO -> Client über die vom Client offengehaltene/gepollt-aufgebaute TCP-Verbindung zugestellt, da Android 14+/VPN ausgehende GO-Sockets mit EACCES blockiert)
static OUTGOING_TCP_QUEUE: Mutex<VecDeque<String>> = Mutex::new(VecDeque::new());
static TCP_SERVER_RUNNING: Mutex<bool> = Mutex::new(false);
static IS_CURRENTLY_GO: Mutex<bool> = Mutex::new(false);

pub const RUSTLE_TCP_PORT: u16 = 4242;

pub fn init_core() -> Result<String, String> {
    let mut wifi = WIFI_MANAGER.lock().map_err(|e| e.to_string())?;
    *wifi = Some(WifiDirectManager::new());

    let mut ble = BLE_MANAGER.lock().map_err(|e| e.to_string())?;
    *ble = Some(PlatformBleManager::new());

    ensure_tcp_server_running();

    Ok("Rust Core (BLE + Wi-Fi + TCP :4242) initialisiert".into())
}

pub fn ble_start_broadcast(device_name: String, is_go: bool) -> Result<String, String> {
    ensure_tcp_server_running();
    *IS_CURRENTLY_GO.lock().unwrap() = is_go;
    if is_go {
        *PEER_IP.lock().unwrap() = None;
        OUTGOING_TCP_QUEUE.lock().unwrap().clear();
    }
    let mut ble = BLE_MANAGER.lock().map_err(|e| e.to_string())?;
    if let Some(ref mut mgr) = *ble {
        mgr.start_advertising(&device_name, is_go)
    } else {
        Err("BLE Manager nicht initialisiert".into())
    }
}

pub fn ble_stop_broadcast() -> Result<String, String> {
    let mut ble = BLE_MANAGER.lock().map_err(|e| e.to_string())?;
    if let Some(ref mut mgr) = *ble {
        mgr.stop_advertising()
    } else {
        Err("BLE Manager nicht initialisiert".into())
    }
}

pub fn ble_scan(duration_secs: u64) -> Result<Vec<DiscoveredPeer>, String> {
    let mut ble = BLE_MANAGER.lock().map_err(|e| e.to_string())?;
    if let Some(ref mut mgr) = *ble {
        mgr.scan_peers(duration_secs)
    } else {
        Err("BLE Manager nicht initialisiert".into())
    }
}

/// Auf dem GO: Prüft auf Paket 2 -> Startet Hotspot & TCP-Server -> Stellt Paket 3 (SSID;PSK;IP) bereit
pub fn ble_poll_and_handle_go() -> Result<Option<String>, String> {
    let incoming = {
        let mut ble = BLE_MANAGER.lock().map_err(|e| e.to_string())?;
        let mgr = ble.as_mut().ok_or("BLE Manager nicht initialisiert")?;
        mgr.poll_incoming_request()?
    };

    if let Some(req) = incoming {
        *IS_CURRENTLY_GO.lock().unwrap() = true;
        ensure_tcp_server_running();

        let creds = {
            let mut wifi = WIFI_MANAGER.lock().map_err(|e| e.to_string())?;
            let w_mgr = wifi.as_mut().ok_or("Wi-Fi Manager nicht initialisiert")?;
            w_mgr.start_hotspot("Rustle_Direct_GO".into(), "password1234".into())?
        };

        if creds.starts_with("ERR:") {
            return Err(creds);
        }

        {
            let mut ble = BLE_MANAGER.lock().map_err(|e| e.to_string())?;
            if let Some(ref mut mgr) = *ble {
                mgr.set_hotspot_credentials(&creds)?;
            }
        }

        return Ok(Some(format!(
            "Anfrage '{}' -> Hotspot & TCP-Server (:4242) bereit! ({})",
            req, creds
        )));
    }

    Ok(None)
}

/// Auf dem Client: Sendet Paket 2 -> Empfängt Paket 3 (SSID;PSK;IP) -> Verbindet Wi-Fi
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

    let wifi_res = {
        let mut wifi = WIFI_MANAGER.lock().map_err(|e| e.to_string())?;
        let w_mgr = wifi.as_mut().ok_or("Wi-Fi Manager nicht initialisiert")?;
        w_mgr.connect_to_hotspot(ssid.clone(), password)?
    };

    if wifi_res.starts_with("ERR:") {
        return Err(wifi_res);
    }

    // Sobald der Client verbunden ist, hält ein Hintergrund-Worker den Kontakt zum GO:
    // 1. Er sendet den INIT_PING (damit der GO die Client-IP sieht)
    // 2. Er fragt periodisch per "POLL" beim GO an, ob der GO eine Nachricht für den Client in der
    //    Warteschlange hat. Dadurch funktioniert GO -> Client selbst dann zuverlässig, wenn auf
    //    Android ein VPN aktiv ist oder Android 14+ ausgehende Sockets auf tethering/ap-Interfaces
    //    mit EACCES (Permission denied) blockiert!
    let target = go_ip.clone();
    std::thread::spawn(move || {
        let mut init_done = false;
        loop {
            // Breche ab, falls wir inzwischen selbst zum GO gewechselt sind oder eine andere GO-IP haben
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
                // Wenn der GO eine Nachricht für uns hatte, beginnt die Antwort mit "MSG:"
                if let Some( idx ) = reply.find("MSG:") {
                    let msg_content = reply[idx + 4..].trim().to_string();
                    if !msg_content.is_empty() {
                        println!("[RUST DEBUG] Vom GO über Poll empfangen: {}", msg_content);
                        if let Ok(mut inbox) = INCOMING_TCP_MSG.lock() {
                            *inbox = Some(msg_content);
                        }
                    }
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(800));
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
            // Falls wir GO sind und ein direktes ausgehendes TCP-Connect fehlschlägt (z.B. wegen
            // Android EACCES auf ap/swlan-Interface bei aktivem VPN oder Linux-Client-Firewall),
            // legen wir das Paket in die ausgehende Queue — der Client holt es innerhalb von <0.8s ab!
            if is_go {
                println!(
                    "[RUST DEBUG] Direktes Senden vom GO blockiert ({}), stelle über Client-Rückkanal zu...",
                    err
                );
                OUTGOING_TCP_QUEUE.lock().unwrap().push_back(payload.clone());

                // Warte kurz (bis zu 2.5s), bis der Client die Nachricht beim nächsten Poll abgeholt hat
                for _ in 0..25 {
                    std::thread::sleep(std::time::Duration::from_millis(100));
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
