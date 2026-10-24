use crate::platform::{
    BleDevice, DiscoveredPeer, PlatformBleManager, WifiDirectDevice, WifiDirectManager,
};
use std::io::{Read, Write};
use std::net::{TcpListener, /* TcpStream */};
use std::sync::Mutex;
use std::thread;
//use std::time::Duration;

static WIFI_MANAGER: Mutex<Option<WifiDirectManager>> = Mutex::new(None);
static BLE_MANAGER: Mutex<Option<PlatformBleManager>> = Mutex::new(None);

// Speichert die IP des GO (wenn wir Client sind) und empfangene TCP-Nachrichten
static TARGET_GO_IP: Mutex<Option<String>> = Mutex::new(None);
static INCOMING_TCP_MSG: Mutex<Option<String>> = Mutex::new(None);
static TCP_SERVER_RUNNING: Mutex<bool> = Mutex::new(false);

pub const RUSTLE_TCP_PORT: u16 = 4242;

pub fn init_core() -> Result<String, String> {
    let mut wifi = WIFI_MANAGER.lock().map_err(|e| e.to_string())?;
    *wifi = Some(WifiDirectManager::new());

    let mut ble = BLE_MANAGER.lock().map_err(|e| e.to_string())?;
    *ble = Some(PlatformBleManager::new());

    Ok("Rust Core (BLE + Wi-Fi) initialisiert".into())
}

pub fn ble_start_broadcast(device_name: String, is_go: bool) -> Result<String, String> {
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
    // Auch falls ein Paket im selben Poll-Zyklus über TCP reinkam, können wir es direkt prüfen:
    let incoming = {
        let mut ble = BLE_MANAGER.lock().map_err(|e| e.to_string())?;
        let mgr = ble.as_mut().ok_or("BLE Manager nicht initialisiert")?;
        mgr.poll_incoming_request()?
    };

    if let Some(req) = incoming {
        let creds = {
            let mut wifi = WIFI_MANAGER.lock().map_err(|e| e.to_string())?;
            let w_mgr = wifi.as_mut().ok_or("Wi-Fi Manager nicht initialisiert")?;
            w_mgr.start_hotspot("Rustle_Direct_GO".into(), "password1234".into())?
        };

        if creds.starts_with("ERR:") {
            return Err(creds);
        }

        // TCP-Server auf Port 4242 starten!
        ensure_tcp_server_running();

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

    let ssid = parts[0].to_string();
    let password = parts[1].to_string();
    let go_ip = if parts.len() >= 3 {
        parts[2].to_string()
    } else {
        "10.42.0.1".to_string()
    };

    // Ziel-IP des GO für spätere TCP-Pakete merken
    *TARGET_GO_IP.lock().unwrap() = Some(go_ip.clone());

    let wifi_res = {
        let mut wifi = WIFI_MANAGER.lock().map_err(|e| e.to_string())?;
        let w_mgr = wifi.as_mut().ok_or("Wi-Fi Manager nicht initialisiert")?;
        w_mgr.connect_to_hotspot(ssid.clone(), password)?
    };

    if wifi_res.starts_with("ERR:") {
        Err(wifi_res)
    } else {
        Ok(format!(
            "Verbunden mit '{}' (GO-IP: {}:{}) -> Bereit für TCP-Pakete!",
            ssid, go_ip, RUSTLE_TCP_PORT
        ))
    }
}

/// Sendet ein echtes TCP-Paket über die Wi-Fi-Verbindung an den GO und liest dessen Antwort
pub fn send_test_packet(payload: String) -> Result<String, String> {
    let go_ip = TARGET_GO_IP
        .lock()
        .unwrap()
        .clone()
        .unwrap_or_else(|| "10.42.0.1".into());

    crate::platform::send_tcp_packet_platform(&go_ip, RUSTLE_TCP_PORT, &payload)
}

/// Fragt auf dem GO ab, ob neue TCP-Pakete eingegangen sind
pub fn poll_received_packets() -> Result<Option<String>, String> {
    let mut lock = INCOMING_TCP_MSG.lock().map_err(|e| e.to_string())?;
    Ok(lock.take())
}

/// Startet im Hintergrund einen TCP-Server auf 0.0.0.0:4242, der Pakete empfängt und sofort ein Echo/ACK zurückschickt
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
                println!("TCP Bind Fehler: {}", e);
                return;
            }
        };

        println!("Rust TCP-Server lauscht auf 0.0.0.0:{}", RUSTLE_TCP_PORT);

        for stream in listener.incoming().flatten() {
            let mut s = stream;
            let peer_addr = s
                .peer_addr()
                .map(|a| a.to_string())
                .unwrap_or_else(|_| "Unbekannt".into());

            let mut buf = [0u8; 4096];
            if let Ok(n) = s.read(&mut buf) {
                if n > 0 {
                    let msg = String::from_utf8_lossy(&buf[..n]).to_string();
                    println!("TCP Paket empfangen von {}: {}", peer_addr, msg);

                    // Für die Flutter-UI speichern
                    if let Ok(mut inbox) = INCOMING_TCP_MSG.lock() {
                        *inbox = Some(format!("Von {}: {}", peer_addr, msg));
                    }

                    // Sofortige Bestätigung über denselben TCP-Stream zurücksenden
                    let reply = format!("ACK vom GO: Habe '{}' ({} Bytes) erhalten!", msg, n);
                    let _ = s.write_all(reply.as_bytes());
                }
            }
        }
    });
}