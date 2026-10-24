use super::{
    BleDevice, DiscoveredPeer, WifiDirectDevice, RUSTLE_ACK_CHAR_UUID, RUSTLE_SERVICE_UUID,
    RUSTLE_SYN_CHAR_UUID,
};
use bluer::adv::{Advertisement, AdvertisementHandle};
use bluer::gatt::local::{
    Application, ApplicationHandle, Characteristic, CharacteristicRead, CharacteristicWrite,
    CharacteristicWriteMethod, Service,
};
use bluer::{Address, AddressType, Uuid};
use std::collections::{BTreeSet, HashMap};
use std::net::Ipv4Addr;
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use zbus::{zvariant::Value, Connection};

pub struct WifiDirectManager;

impl WifiDirectManager {
    pub fn new() -> Self {
        Self
    }
}

impl WifiDirectDevice for WifiDirectManager {
    fn start_discovery(&mut self) -> Result<(), String> {
        Ok(())
    }

    fn connect_to(&mut self, _peer_id: String) -> Result<(), String> {
        Ok(())
    }

    fn start_hotspot(&mut self, ssid: String, psk: String) -> Result<String, String> {
        // Erzwinge "DIRECT-" Präfix gemäß Wi-Fi Direct Spezifikation, damit Android WifiP2pManager
        // sich über das P2P-Interface (p2p-wlan0-x) verbindet und das Heim-WLAN (wlan0) aktiv bleibt!
        let direct_ssid = if ssid.starts_with("DIRECT-") {
            ssid
        } else {
            format!("DIRECT-{}", ssid)
        };
        async_std_or_tokio_block(async {
            let ip = start_hotspot_internal(&direct_ssid, &psk)
                .await
                .map_err(|e| e.to_string())?;
            println!("[LINUX WIFI] Wi-Fi Direct GO bereit: SSID={}, IP={}", direct_ssid, ip);
            Ok(format!("{};{};{}", direct_ssid, psk, ip))
        })
    }

    fn connect_to_hotspot(&mut self, ssid: String, password: String) -> Result<String, String> {
        async_std_or_tokio_block(async {
            connect_to_hotspot_internal(&ssid, &password)
                .await
                .map_err(|e| e.to_string())
        })
    }
}

fn async_std_or_tokio_block<F, T>(future: F) -> T
where
    F: std::future::Future<Output = T>,
{
    futures_lite::future::block_on(future)
}

/// Liest die lokale Hotspot-IP (z.B. 10.42.0.1) von den Linux-Netzwerkinterfaces aus,
/// nachdem NetworkManager den Shared-AP gestartet hat.
fn resolve_local_linux_hotspot_ip() -> Option<String> {
    for _ in 0..25 {
        if let Ok(output) = std::process::Command::new("ip")
            .args(["-4", "-o", "addr", "show"])
            .output()
        {
            let text = String::from_utf8_lossy(&output.stdout);
            for line in text.lines() {
                if line.contains("10.42.") {
                    for part in line.split_whitespace() {
                        if part.starts_with("10.42.") {
                            if let Some(ip) = part.split('/').next() {
                                return Some(ip.to_string());
                            }
                        }
                    }
                }
            }
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    None
}

async fn start_hotspot_internal(
    ssid: &str,
    psk: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    // 1. Alten Hotspot wegräumen, falls noch ein Profil namens "Rust_Hotspot" existiert
    let _ = std::process::Command::new("nmcli")
        .args(["connection", "delete", "Rust_Hotspot"])
        .output();

    // 2. Ermittle das WLAN-Interface über nmcli (z.B. wlp11s0)
    let mut wifi_ifname: Option<String> = None;
    if let Ok(out) = std::process::Command::new("nmcli")
        .args(["-t", "-f", "DEVICE,TYPE", "device"])
        .output()
    {
        let stdout = String::from_utf8_lossy(&out.stdout);
        for line in stdout.lines() {
            if let Some((dev, dtype)) = line.split_once(':') {
                if dtype.trim() == "wifi" && !dev.starts_with("p2p") {
                    wifi_ifname = Some(dev.trim().to_string());
                    break;
                }
            }
        }
    }

    // 3. Erzeuge das Wi-Fi Direct GO / Hotspot-Profil mit reinem WPA2-PSK (pmf=1 disable)
    //    und connection.zone=trusted (damit firewalld auf Linux Port 4242 nicht blockiert!).
    if let Some(ref ifname) = wifi_ifname {
        println!(
            "[LINUX WIFI] Erstelle Wi-Fi Direct GO ('{}') auf Interface '{}' via nmcli...",
            ssid, ifname
        );
        let add_res = std::process::Command::new("nmcli")
            .args([
                "connection",
                "add",
                "type",
                "wifi",
                "ifname",
                ifname,
                "con-name",
                "Rust_Hotspot",
                "autoconnect",
                "no",
                "ssid",
                ssid,
                "connection.zone",
                "trusted",
                "802-11-wireless.mode",
                "ap",
                "802-11-wireless.band",
                "bg",
                "ipv4.method",
                "shared",
                "wifi-sec.key-mgmt",
                "wpa-psk",
                "wifi-sec.psk",
                psk,
                "wifi-sec.proto",
                "rsn",
                "wifi-sec.pairwise",
                "ccmp",
                "wifi-sec.group",
                "ccmp",
                "wifi-sec.pmf",
                "1", // 1 = disable PMF (reines WPA2-PSK für WifiP2pManager!)
                "wifi-sec.wps-method",
                "1", // 1 = disabled
            ])
            .output();

        if let Ok(output) = add_res {
            if output.status.success() {
                let up_res = std::process::Command::new("nmcli")
                    .args(["connection", "up", "Rust_Hotspot"])
                    .output();
                if let Ok(up_out) = up_res {
                    if up_out.status.success() {
                        let ip = resolve_local_linux_hotspot_ip()
                            .unwrap_or_else(|| "10.42.0.1".to_string());
                        return Ok(ip);
                    } else {
                        println!(
                            "[LINUX WIFI] nmcli up Warnung: {}",
                            String::from_utf8_lossy(&up_out.stderr)
                        );
                    }
                }
            } else {
                println!(
                    "[LINUX WIFI] nmcli add Warnung: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }
        }
    }

    // 4. Fallback über D-Bus
    let connection = Connection::system().await?;
    let mut settings: HashMap<&str, HashMap<&str, Value>> = HashMap::new();

    let mut conn_settings: HashMap<&str, Value> = HashMap::new();
    conn_settings.insert("id", Value::from("Rust_Hotspot"));
    conn_settings.insert("type", Value::from("802-11-wireless"));
    conn_settings.insert("autoconnect", Value::from(false));
    conn_settings.insert("zone", Value::from("trusted"));
    settings.insert("connection", conn_settings);

    let mut wifi_settings: HashMap<&str, Value> = HashMap::new();
    wifi_settings.insert("mode", Value::from("ap"));
    wifi_settings.insert("band", Value::from("bg"));
    wifi_settings.insert("ssid", Value::from(ssid.as_bytes().to_vec()));
    settings.insert("802-11-wireless", wifi_settings);

    let mut sec_settings: HashMap<&str, Value> = HashMap::new();
    sec_settings.insert("key-mgmt", Value::from("wpa-psk"));
    sec_settings.insert("psk", Value::from(psk));
    sec_settings.insert("pmf", Value::from(1i32));
    settings.insert("802-11-wireless-security", sec_settings);

    let mut ip4_settings: HashMap<&str, Value> = HashMap::new();
    ip4_settings.insert("method", Value::from("shared"));
    settings.insert("ipv4", ip4_settings);

    let proxy = zbus::Proxy::new(
        &connection,
        "org.freedesktop.NetworkManager",
        "/org/freedesktop/NetworkManager/Settings",
        "org.freedesktop.NetworkManager.Settings",
    )
    .await?;

    let path: zbus::zvariant::OwnedObjectPath = proxy.call("AddConnection", &(settings)).await?;

    let nm_proxy = zbus::Proxy::new(
        &connection,
        "org.freedesktop.NetworkManager",
        "/org/freedesktop/NetworkManager",
        "org.freedesktop.NetworkManager",
    )
    .await?;

    let _active_path: zbus::zvariant::OwnedObjectPath = nm_proxy
        .call(
            "ActivateConnection",
            &(
                &path,
                zbus::zvariant::ObjectPath::from_str_unchecked("/"),
                zbus::zvariant::ObjectPath::from_str_unchecked("/"),
            ),
        )
        .await?;

    let ip = resolve_local_linux_hotspot_ip().unwrap_or_else(|| "10.42.0.1".to_string());
    Ok(ip)
}

async fn connect_to_hotspot_internal(
    ssid: &str,
    password: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    // Lösche eventuell vorhandene alte Verbindungsprofile mit gleicher SSID
    let _ = std::process::Command::new("nmcli")
        .args(["connection", "delete", ssid])
        .output();

    let connection = Connection::system().await?;

    let mut settings: HashMap<&str, HashMap<&str, Value>> = HashMap::new();

    let mut conn_settings: HashMap<&str, Value> = HashMap::new();
    conn_settings.insert("id", Value::from(ssid));
    conn_settings.insert("type", Value::from("802-11-wireless"));
    // Setze Firewall-Zone auf "trusted", damit eingehende Pakete vom Android-GO auf Port 4242
    // nicht von firewalld auf Linux verworfen werden!
    conn_settings.insert("zone", Value::from("trusted"));
    settings.insert("connection", conn_settings);

    let mut wifi_settings: HashMap<&str, Value> = HashMap::new();
    wifi_settings.insert("mode", Value::from("infrastructure"));
    wifi_settings.insert("ssid", Value::from(ssid.as_bytes().to_vec()));
    settings.insert("802-11-wireless", wifi_settings);

    let mut sec_settings: HashMap<&str, Value> = HashMap::new();
    sec_settings.insert("key-mgmt", Value::from("wpa-psk"));
    sec_settings.insert("psk", Value::from(password));
    settings.insert("802-11-wireless-security", sec_settings);

    let nm_proxy = zbus::Proxy::new(
        &connection,
        "org.freedesktop.NetworkManager",
        "/org/freedesktop/NetworkManager",
        "org.freedesktop.NetworkManager",
    )
    .await?;

    let (_path, active_path): (
        zbus::zvariant::OwnedObjectPath,
        zbus::zvariant::OwnedObjectPath,
    ) = nm_proxy
        .call(
            "AddAndActivateConnection",
            &(
                settings,
                zbus::zvariant::ObjectPath::from_str_unchecked("/"),
                zbus::zvariant::ObjectPath::from_str_unchecked("/"),
            ),
        )
        .await?;

    // Warte bis zu 12 Sekunden, bis das Interface per DHCP tatsächlich eine IPv4-Adresse
    // vom Android-GO (192.168.49.x bzw. 10.x.x.x) erhalten hat!
    for i in 1..=24 {
        std::thread::sleep(Duration::from_millis(500));
        if let Ok(ac_proxy) = zbus::Proxy::new(
            &connection,
            "org.freedesktop.NetworkManager",
            active_path.as_str(),
            "org.freedesktop.NetworkManager.Connection.Active",
        )
        .await
        {
            // State 2 == NM_ACTIVE_CONNECTION_STATE_ACTIVATED
            if ac_proxy.get_property::<u32>("State").await.unwrap_or(0) == 2 {
                println!(
                    "[LINUX WIFI] Verbindung zu '{}' nach {}ms vollständig aktiviert (DHCP fertig)!",
                    ssid,
                    i * 500
                );
                break;
            }
        }
    }

    Ok(format!("Mit '{}' verbunden ({:?})", ssid, active_path))
}

// -----------------------------------
// BLE Manager (Linux via BlueZ / bluer)
// -----------------------------------

pub struct LinuxBleManager {
    rt: tokio::runtime::Runtime,
    adv_handle: Option<AdvertisementHandle>,
    app_handle: Option<ApplicationHandle>,
    incoming_request: Arc<Mutex<Option<String>>>,
    hotspot_credentials: Arc<Mutex<String>>,
}

impl LinuxBleManager {
    pub fn new() -> Self {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("Konnte Tokio Runtime nicht starten");
        Self {
            rt,
            adv_handle: None,
            app_handle: None,
            incoming_request: Arc::new(Mutex::new(None)),
            hotspot_credentials: Arc::new(Mutex::new(String::new())),
        }
    }
}

impl BleDevice for LinuxBleManager {
    fn start_advertising(&mut self, device_name: &str, is_go: bool) -> Result<String, String> {
        let role_prefix = if is_go { "R_GO_" } else { "R_CL_" };
        let short_name: String = device_name.chars().take(4).collect();
        let local_name = format!("{}{}", role_prefix, short_name);
        let local_name_for_return = local_name.clone();

        *self.incoming_request.lock().unwrap() = None;
        *self.hotspot_credentials.lock().unwrap() = String::new();

        let incoming_clone = self.incoming_request.clone();
        let creds_clone = self.hotspot_credentials.clone();

        let (adv_handle, app_handle) = self.rt.block_on(async move {
            let session = bluer::Session::new().await.map_err(|e| e.to_string())?;
            let adapter = session.default_adapter().await.map_err(|e| e.to_string())?;
            adapter.set_powered(true).await.map_err(|e| e.to_string())?;

            // Charakteristik 1: Briefkasten für Paket 2 (Write)
            let syn_char = Characteristic {
                uuid: Uuid::from_u128(RUSTLE_SYN_CHAR_UUID),
                write: Some(CharacteristicWrite {
                    write: true,
                    write_without_response: true,
                    method: CharacteristicWriteMethod::Fun(Box::new(move |new_value, _req| {
                        let incoming = incoming_clone.clone();
                        Box::pin(async move {
                            if let Ok(msg) = String::from_utf8(new_value) {
                                println!("Linux GATT empfing Paket 2: {}", msg);
                                if let Ok(mut lock) = incoming.lock() {
                                    *lock = Some(msg);
                                }
                            }
                            Ok(())
                        })
                    })),
                    ..Default::default()
                }),
                ..Default::default()
            };

            // Charakteristik 2: Ausgabefach für Paket 3 "SSID;PASSWORT;IP" (Read)
            let ack_char = Characteristic {
                uuid: Uuid::from_u128(RUSTLE_ACK_CHAR_UUID),
                read: Some(CharacteristicRead {
                    read: true,
                    fun: Box::new(move |_req| {
                        let creds = creds_clone.clone();
                        Box::pin(async move {
                            let val = creds
                                .lock()
                                .map(|c| c.as_bytes().to_vec())
                                .unwrap_or_default();
                            Ok(val)
                        })
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            };

            let service = Service {
                uuid: Uuid::from_u128(RUSTLE_SERVICE_UUID),
                primary: true,
                characteristics: vec![syn_char, ack_char],
                ..Default::default()
            };

            let app = Application {
                services: vec![service],
                ..Default::default()
            };

            let app_h = adapter
                .serve_gatt_application(app)
                .await
                .map_err(|e| e.to_string())?;

            let mut service_uuids = BTreeSet::new();
            service_uuids.insert(Uuid::from_u128(RUSTLE_SERVICE_UUID));

            let le_advertisement = Advertisement {
                advertisement_type: bluer::adv::Type::Peripheral,
                service_uuids,
                local_name: Some(local_name),
                ..Default::default()
            };

            let adv_h = adapter
                .advertise(le_advertisement)
                .await
                .map_err(|e| e.to_string())?;
            Ok::<_, String>((adv_h, app_h))
        })?;

        self.adv_handle = Some(adv_handle);
        self.app_handle = Some(app_handle);
        Ok(format!(
            "Linux GO bereit ('{}') - Wartet auf Paket 2...",
            local_name_for_return
        ))
    }

    fn stop_advertising(&mut self) -> Result<String, String> {
        self.adv_handle = None;
        self.app_handle = None;
        Ok("Linux BLE Advertising & GATT-Server gestoppt".into())
    }

    fn scan_peers(&mut self, duration_secs: u64) -> Result<Vec<DiscoveredPeer>, String> {
        self.rt.block_on(async {
            let session = bluer::Session::new().await.map_err(|e| e.to_string())?;
            let adapter = session.default_adapter().await.map_err(|e| e.to_string())?;
            adapter.set_powered(true).await.map_err(|e| e.to_string())?;

            let filter = bluer::DiscoveryFilter {
                transport: bluer::DiscoveryTransport::Le,
                ..Default::default()
            };
            let _ = adapter.set_discovery_filter(filter).await;

            let _discover = adapter.discover_devices().await.map_err(|e| e.to_string())?;
            tokio::time::sleep(Duration::from_secs(duration_secs)).await;

            let mut peers = Vec::new();
            let device_addresses = adapter.device_addresses().await.map_err(|e| e.to_string())?;

            for addr in device_addresses {
                if let Ok(dev) = adapter.device(addr) {
                    let addr_type = dev.address_type().await.ok();
                    let name = dev.name().await.ok().flatten().unwrap_or_default();
                    let rssi = dev.rssi().await.ok().flatten().unwrap_or(0);
                    let uuids = dev.uuids().await.ok().flatten().unwrap_or_default();

                    let has_our_uuid = uuids.contains(&Uuid::from_u128(RUSTLE_SERVICE_UUID));
                    let is_rustle_name = name.starts_with("R_GO_") || name.starts_with("R_CL_");

                    if has_our_uuid
                        || is_rustle_name
                        || (!name.is_empty() && addr_type != Some(AddressType::BrEdr))
                    {
                        let is_go = name.starts_with("R_GO_");
                        peers.push(DiscoveredPeer {
                            address: addr.to_string(),
                            name: if name.is_empty() {
                                "<Unbekannt>".into()
                            } else {
                                name
                            },
                            is_go,
                            rssi,
                        });
                    }
                }
            }
            Ok(peers)
        })
    }

    fn send_connection_request(
        &mut self,
        target_address: &str,
        client_name: &str,
    ) -> Result<String, String> {
        let addr =
            Address::from_str(target_address).map_err(|e| format!("Ungültige MAC: {}", e))?;
        let payload = format!("CONNECT;{}", client_name);

        self.rt.block_on(async move {
            let session = bluer::Session::new().await.map_err(|e| e.to_string())?;
            let adapter = session.default_adapter().await.map_err(|e| e.to_string())?;
            let mut device = adapter.device(addr).map_err(|e| e.to_string())?;

            if !device.is_connected().await.unwrap_or(false) {
                if let Err(first_err) = device.connect().await {
                    let err_msg = first_err.to_string();
                    println!(
                        "[LINUX BLE] Erster Connect-Versuch fehlgeschlagen ({}), bereinige BlueZ-Cache für {}...",
                        err_msg, addr
                    );
                    let _ = adapter.remove_device(addr).await;

                    let filter = bluer::DiscoveryFilter {
                        transport: bluer::DiscoveryTransport::Le,
                        ..Default::default()
                    };
                    let _ = adapter.set_discovery_filter(filter).await;

                    if let Ok(_disc) = adapter.discover_devices().await {
                        for _ in 0..15 {
                            tokio::time::sleep(Duration::from_millis(250)).await;
                            if let Ok(addrs) = adapter.device_addresses().await {
                                if addrs.contains(&addr) {
                                    break;
                                }
                            }
                        }
                    }

                    device = adapter.device(addr).map_err(|e| e.to_string())?;
                    device.connect().await.map_err(|e| {
                        format!(
                            "BLE Connect fehlgeschlagen (trotz LE-Cache-Reset nach '{}'): {}",
                            err_msg, e
                        )
                    })?;
                }
            }

            for _ in 0..15 {
                if device.is_services_resolved().await.unwrap_or(false) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(300)).await;
            }

            let target_service = Uuid::from_u128(RUSTLE_SERVICE_UUID);
            let syn_uuid = Uuid::from_u128(RUSTLE_SYN_CHAR_UUID);
            let ack_uuid = Uuid::from_u128(RUSTLE_ACK_CHAR_UUID);

            let mut syn_char_found = None;
            let mut ack_char_found = None;

            for service in device.services().await.map_err(|e| e.to_string())? {
                if service.uuid().await.ok() == Some(target_service) {
                    for ch in service.characteristics().await.map_err(|e| e.to_string())? {
                        let u = ch.uuid().await.ok();
                        if u == Some(syn_uuid) {
                            syn_char_found = Some(ch);
                        } else if u == Some(ack_uuid) {
                            ack_char_found = Some(ch);
                        }
                    }
                }
            }

            let syn_char = syn_char_found.ok_or("SYN-Charakteristik auf GO nicht gefunden")?;
            let ack_char = ack_char_found.ok_or("ACK-Charakteristik auf GO nicht gefunden")?;

            // 1. Paket 2 schreiben
            syn_char
                .write(payload.as_bytes())
                .await
                .map_err(|e| format!("GATT Write Fehler: {}", e))?;

            // 2. Auf Paket 3 (SSID;PASSWORT;IP) warten (bis zu 25 Sekunden)
            for _ in 0..50 {
                tokio::time::sleep(Duration::from_millis(500)).await;
                if let Ok(bytes) = ack_char.read().await {
                    if let Ok(creds) = String::from_utf8(bytes) {
                        if creds.contains(';') {
                            println!("[LINUX BLE] Paket 3 empfangen: {}", creds);
                            let _ = device.disconnect().await;
                            return Ok(creds);
                        }
                    }
                }
            }

            let _ = device.disconnect().await;
            Err("Timeout: GO hat keine Wi-Fi Zugangsdaten (Paket 3) bereitgestellt".into())
        })
    }

    fn poll_incoming_request(&mut self) -> Result<Option<String>, String> {
        let mut lock = self.incoming_request.lock().map_err(|e| e.to_string())?;
        Ok(lock.take())
    }

    fn set_hotspot_credentials(&mut self, credentials: &str) -> Result<(), String> {
        let mut lock = self.hotspot_credentials.lock().map_err(|e| e.to_string())?;
        *lock = credentials.to_string();
        Ok(())
    }
}

/// Findet die lokale IPv4-Adresse des Linux-Rechners, die im selben /24-Subnetz wie `target_ip` liegt.
fn find_local_ipv4_in_same_subnet(target_ip: &str) -> Option<Ipv4Addr> {
    let parts: Vec<&str> = target_ip.trim().split('.').collect();
    if parts.len() != 4 {
        return None;
    }
    let prefix = format!("{}.{}.{}.", parts[0], parts[1], parts[2]);

    if let Ok(output) = std::process::Command::new("ip")
        .args(["-4", "-o", "addr", "show"])
        .output()
    {
        let text = String::from_utf8_lossy(&output.stdout);
        for line in text.lines() {
            for token in line.split_whitespace() {
                if token.starts_with(&prefix) {
                    if let Some(ip_str) = token.split('/').next() {
                        if let Ok(addr) = ip_str.parse::<Ipv4Addr>() {
                            return Some(addr);
                        }
                    }
                }
            }
        }
    }
    None
}

pub fn send_tcp_packet_platform(
    target_ip: &str,
    port: u16,
    message: &str,
) -> Result<String, String> {
    use std::io::{Read, Write};
    use std::net::{IpAddr, SocketAddr, TcpStream};

    let clean_ip = target_ip.trim();
    let parsed_ip: Ipv4Addr = clean_ip
        .parse()
        .map_err(|e| format!("Ungültige Ziel-IPv4 '{}': {}", clean_ip, e))?;
    let sock_addr = SocketAddr::new(IpAddr::V4(parsed_ip), port);

    if let Some(local_ip) = find_local_ipv4_in_same_subnet(clean_ip) {
        println!(
            "[LINUX TCP] Verbinde zu {} (lokale Subnetz-IP: {})",
            sock_addr, local_ip
        );
    } else {
        println!("[LINUX TCP] Verbinde zu {}", sock_addr);
    }

    let mut stream = TcpStream::connect_timeout(&sock_addr, Duration::from_secs(5))
        .map_err(|e| format!("Linux TCP-Connect zu {} fehlgeschlagen: {}", sock_addr, e))?;

    stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
    stream
        .write_all(message.as_bytes())
        .map_err(|e| format!("Senden fehlgeschlagen: {}", e))?;
    let _ = stream.flush();

    let mut buf = [0u8; 4096];
    let n = stream
        .read(&mut buf)
        .map_err(|e| format!("Keine Antwort vom Peer: {}", e))?;

    let reply = String::from_utf8_lossy(&buf[..n]).to_string();
    Ok(format!("Antwort vom Peer: {}", reply))
}
