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
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::net::Ipv4Addr;
use std::str::FromStr;
use std::sync::{Arc, Mutex, Once};
use std::time::Duration;
use zbus::{zvariant::Value, Connection};

pub const RUSTLE_COMPANY_ID: u16 = 0x5255;

// Speichert das vor dem Rustle-Start aktive normale WLAN-Profil (Name, Interface),
// damit es beim Beenden der App oder beim Reset automatisch wiederhergestellt wird.
static PREVIOUS_WIFI_STATE: Mutex<Option<(String, String)>> = Mutex::new(None);
// Speichert eine temporär als Client angelegte Wi-Fi Direct Verbindung (z.B. DIRECT-AndroidGO)
static ACTIVE_CLIENT_SSID: Mutex<Option<String>> = Mutex::new(None);
// Merkt sich, ob wir einen Hotspot (Rust_Hotspot) gestartet haben
static HOTSPOT_ACTIVE: Mutex<bool> = Mutex::new(false);
// Cache für verbindungslose Wi-Fi Direct Zugangsdaten aus BLE Manufacturer Data (0x5255)
static DISCOVERED_GO_CREDENTIALS: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);

static EXIT_HOOK_INIT: Once = Once::new();

extern "C" {
    fn atexit(cb: extern "C" fn()) -> std::os::raw::c_int;
}

extern "C" fn on_linux_process_exit() {
    let _ = cleanup_linux_wifi_sync();
}

fn register_exit_cleanup_once() {
    EXIT_HOOK_INIT.call_once(|| unsafe {
        let _ = atexit(on_linux_process_exit);
    });
}

fn cache_go_credentials(addr: &str, creds: String) {
    if let Ok(mut lock) = DISCOVERED_GO_CREDENTIALS.lock() {
        if lock.is_none() {
            *lock = Some(HashMap::new());
        }
        if let Some(ref mut map) = *lock {
            map.insert(addr.to_string(), creds);
        }
    }
}

fn get_cached_go_credentials(addr: &str) -> Option<String> {
    if let Ok(lock) = DISCOVERED_GO_CREDENTIALS.lock() {
        if let Some(ref map) = *lock {
            return map.get(addr).cloned();
        }
    }
    None
}

/// Packt SSID, PSK und IPv4 in ein kompaktes Byte-Array (<= 24 Bytes)
/// für BLE Connectionless Advertising im Manufacturer Data Feld (0x5255).
pub fn pack_wifi_credentials(ssid: &str, psk: &str, ip: &str) -> Option<Vec<u8>> {
    let clean_ip = ip.trim();
    let parsed_ip: Ipv4Addr = clean_ip.parse().ok()?;
    let ip_bytes = parsed_ip.octets();

    let clean_ssid = ssid.trim_matches('"');
    let suffix = if clean_ssid.starts_with("DIRECT-") {
        &clean_ssid[7..]
    } else {
        clean_ssid
    };

    let psk_bytes = psk.as_bytes();
    let suffix_bytes = suffix.as_bytes();

    if psk_bytes.len() > 16 || suffix_bytes.len() > 16 {
        return None;
    }

    let total_len = 1 + 4 + 1 + psk_bytes.len() + suffix_bytes.len();
    if total_len > 24 {
        return None;
    }

    let mut result = Vec::with_capacity(total_len);
    result.push(0x52); // Magic 'R'
    result.extend_from_slice(&ip_bytes);
    result.push(psk_bytes.len() as u8);
    result.extend_from_slice(psk_bytes);
    result.extend_from_slice(suffix_bytes);
    Some(result)
}

/// Entpackt empfangene Manufacturer Data Bytes zurück in den String "SSID;PSK;IP".
pub fn unpack_wifi_credentials(data: &[u8]) -> Option<String> {
    if data.len() < 7 || data[0] != 0x52 {
        return None;
    }

    let ip = format!("{}.{}.{}.{}", data[1], data[2], data[3], data[4]);
    let psk_len = data[5] as usize;
    if data.len() < 6 + psk_len {
        return None;
    }

    let psk = std::str::from_utf8(&data[6..6 + psk_len]).ok()?;
    let suffix = std::str::from_utf8(&data[6 + psk_len..]).ok()?;
    let ssid = if suffix.starts_with("DIRECT-") {
        suffix.to_string()
    } else {
        format!("DIRECT-{}", suffix)
    };

    Some(format!("{};{};{}", ssid, psk, ip))
}

/// Merkt sich die aktuell aktive normale WLAN-Verbindung (falls noch nicht gespeichert),
/// bevor Rustle das Interface auf Hotspot oder Wi-Fi Direct Client umschaltet.
fn save_previous_wifi_connection() {
    let mut prev_lock = match PREVIOUS_WIFI_STATE.lock() {
        Ok(l) => l,
        Err(_) => return,
    };
    if prev_lock.is_some() {
        return;
    }

    if let Ok(out) = std::process::Command::new("nmcli")
        .args(["-t", "-f", "NAME,TYPE,DEVICE", "connection", "show", "--active"])
        .output()
    {
        let stdout = String::from_utf8_lossy(&out.stdout);
        for line in stdout.lines() {
            let parts: Vec<&str> = line.split(':').collect();
            if parts.len() >= 3 {
                let name = parts[0].trim();
                let ctype = parts[1].trim();
                let dev = parts[2].trim();
                if ctype == "802-11-wireless"
                    && name != "Rust_Hotspot"
                    && !name.starts_with("DIRECT-")
                    && !dev.is_empty()
                {
                    println!(
                        "[LINUX WIFI] Merke vorheriges WLAN-Profil '{}' auf Interface '{}' für späteren Restore",
                        name, dev
                    );
                    *prev_lock = Some((name.to_string(), dev.to_string()));
                    break;
                }
            }
        }
    }
}

/// Räumt den Rustle-Hotspot bzw. temporäre Client-Verbindungen auf und verbindet Linux
/// wieder mit dem vorherigen normalen Heim-WLAN.
pub fn cleanup_linux_wifi_sync() -> String {
    let mut actions = Vec::new();

    let was_hotspot = HOTSPOT_ACTIVE.lock().map(|mut h| {
        let v = *h;
        *h = false;
        v
    }).unwrap_or(false);

    // 1. Temporären Hotspot löschen (falls aktiv oder noch in NetworkManager vorhanden)
    let hs_del = std::process::Command::new("nmcli")
        .args(["connection", "delete", "Rust_Hotspot"])
        .output();
    if was_hotspot || hs_del.map(|o| o.status.success()).unwrap_or(false) {
        println!("[LINUX WIFI] Profil 'Rust_Hotspot' entfernt.");
        actions.push("Hotspot gestoppt".to_string());
    }

    // 2. Temporäres Client-Profil (z.B. DIRECT-AndroidGO) löschen
    if let Ok(mut client_lock) = ACTIVE_CLIENT_SSID.lock() {
        if let Some(ssid) = client_lock.take() {
            let _ = std::process::Command::new("nmcli")
                .args(["connection", "delete", &ssid])
                .output();
            println!("[LINUX WIFI] Temporäres Client-Profil '{}' entfernt.", ssid);
            actions.push(format!("Client-Profil '{}' entfernt", ssid));
        }
    }

    // 3. Vorheriges normales WLAN wieder aktivieren
    if let Ok(mut prev_lock) = PREVIOUS_WIFI_STATE.lock() {
        if let Some((prev_name, prev_dev)) = prev_lock.take() {
            println!(
                "[LINUX WIFI] Stelle vorherige WLAN-Verbindung '{}' auf '{}' wieder her...",
                prev_name, prev_dev
            );
            let up_res = std::process::Command::new("nmcli")
                .args(["connection", "up", &prev_name])
                .output();
            if up_res.map(|o| o.status.success()).unwrap_or(false) {
                actions.push(format!("WLAN '{}' wiederhergestellt", prev_name));
            } else {
                let _ = std::process::Command::new("nmcli")
                    .args(["device", "connect", &prev_dev])
                    .output();
                actions.push(format!("Interface '{}' für Auto-Connect freigegeben", prev_dev));
            }
        }
    }

    if actions.is_empty() {
        "Netzwerk bereits im Normalzustand".into()
    } else {
        actions.join(", ")
    }
}

pub struct WifiDirectManager;

impl WifiDirectManager {
    pub fn new() -> Self {
        register_exit_cleanup_once();
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
        register_exit_cleanup_once();
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
        register_exit_cleanup_once();
        async_std_or_tokio_block(async {
            connect_to_hotspot_internal(&ssid, &password)
                .await
                .map_err(|e| e.to_string())
        })
    }

    fn stop_hotspot(&mut self) -> Result<String, String> {
        Ok(cleanup_linux_wifi_sync())
    }
}

fn async_std_or_tokio_block<F, T>(future: F) -> T
where
    F: std::future::Future<Output = T>,
{
    futures_lite::future::block_on(future)
}

/// Liest die lokale Hotspot-IP (z.B. 10.42.0.1) schnell von den Linux-Netzwerkinterfaces aus,
/// nachdem NetworkManager den Shared-AP gestartet hat.
fn resolve_local_linux_hotspot_ip() -> Option<String> {
    for attempt in 0..35 {
        if attempt > 0 {
            std::thread::sleep(Duration::from_millis(80));
        }
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
    }
    None
}

async fn start_hotspot_internal(
    ssid: &str,
    psk: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    // 0. Vorheriges normales WLAN merken (für späteren Restore beim Beenden)
    save_previous_wifi_connection();

    // Falls der Hotspot bereits aktiv ist und eine 10.42.x.x IP hat, sofort zurückgeben (0ms!)
    if *HOTSPOT_ACTIVE.lock().unwrap() {
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
                                return Ok(ip.to_string());
                            }
                        }
                    }
                }
            }
        }
    }

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
                        *HOTSPOT_ACTIVE.lock().unwrap() = true;
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

    *HOTSPOT_ACTIVE.lock().unwrap() = true;
    let ip = resolve_local_linux_hotspot_ip().unwrap_or_else(|| "10.42.0.1".to_string());
    Ok(ip)
}

async fn connect_to_hotspot_internal(
    ssid: &str,
    password: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    // 0. Vorheriges normales WLAN merken (für späteren Restore beim Beenden)
    save_previous_wifi_connection();

    // Lösche eventuell vorhandene alte Verbindungsprofile mit gleicher SSID
    let _ = std::process::Command::new("nmcli")
        .args(["connection", "delete", ssid])
        .output();

    if let Ok(mut client_lock) = ACTIVE_CLIENT_SSID.lock() {
        *client_lock = Some(ssid.to_string());
    }

    // 1. Ermittle das WLAN-Interface über nmcli (z.B. wlp11s0)
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

    // 2. Primärer Pfad: Schnelle & zuverlässige Verbindung per nmcli (inkl. DHCP IPv4 & trusted zone)
    if let Some(ref ifname) = wifi_ifname {
        println!(
            "[LINUX WIFI] Verbinde als Client zu '{}' auf Interface '{}' via nmcli...",
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
                ssid,
                "autoconnect",
                "no",
                "ssid",
                ssid,
                "connection.zone",
                "trusted",
                "ipv4.method",
                "auto",
                "wifi-sec.key-mgmt",
                "wpa-psk",
                "wifi-sec.psk",
                password,
            ])
            .output();

        if let Ok(output) = add_res {
            if output.status.success() {
                let up_res = std::process::Command::new("nmcli")
                    .args(["connection", "up", ssid])
                    .output();
                if let Ok(up_out) = up_res {
                    if up_out.status.success() {
                        println!("[LINUX WIFI] nmcli up erfolgreich für '{}'", ssid);
                        // Warte kurz, bis IP per DHCP vergeben ist
                        for _ in 0..40 {
                            if find_local_ipv4_in_same_subnet("192.168.49.1").is_some()
                                || find_local_ipv4_in_same_subnet("10.42.0.1").is_some()
                            {
                                break;
                            }
                            std::thread::sleep(Duration::from_millis(100));
                        }
                        return Ok(format!("Mit '{}' verbunden via nmcli", ssid));
                    } else {
                        println!(
                            "[LINUX WIFI] nmcli connection up Warnung: {}",
                            String::from_utf8_lossy(&up_out.stderr)
                        );
                    }
                }
            } else {
                println!(
                    "[LINUX WIFI] nmcli connection add Warnung: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }
        }
    }

    // 3. Fallback über D-Bus (inklusive ipv4 auto)
    let connection = Connection::system().await?;

    let mut settings: HashMap<&str, HashMap<&str, Value>> = HashMap::new();

    let mut conn_settings: HashMap<&str, Value> = HashMap::new();
    conn_settings.insert("id", Value::from(ssid));
    conn_settings.insert("type", Value::from("802-11-wireless"));
    conn_settings.insert("autoconnect", Value::from(false));
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

    let mut ip4_settings: HashMap<&str, Value> = HashMap::new();
    ip4_settings.insert("method", Value::from("auto"));
    settings.insert("ipv4", ip4_settings);

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

    // Schnelles Polling (alle 150ms), bis das Interface per DHCP eine IPv4-Adresse erhalten hat
    for i in 1..=80 {
        std::thread::sleep(Duration::from_millis(150));
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
                    i * 150
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
        register_exit_cleanup_once();
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

impl Drop for LinuxBleManager {
    fn drop(&mut self) {
        self.adv_handle = None;
        self.app_handle = None;
        let _ = cleanup_linux_wifi_sync();
    }
}

impl BleDevice for LinuxBleManager {
    fn start_advertising(&mut self, device_name: &str, is_go: bool) -> Result<String, String> {
        let role_prefix = if is_go { "R_GO_" } else { "R_CL_" };
        let short_name: String = device_name.chars().take(4).collect();
        let local_name = format!("{}{}", role_prefix, short_name);
        let local_name_for_return = local_name.clone();

        *self.incoming_request.lock().unwrap() = None;

        let incoming_clone = self.incoming_request.clone();
        let creds_clone = self.hotspot_credentials.clone();

        let (adv_handle, app_handle) = self.rt.block_on(async move {
            let session = bluer::Session::new().await.map_err(|e| e.to_string())?;
            let adapter = session.default_adapter().await.map_err(|e| e.to_string())?;
            adapter.set_powered(true).await.map_err(|e| e.to_string())?;
            let _ = adapter.set_pairable(false).await;

            // Entferne nicht-verbundene gecachte Geräte ohne aktiven Dienst aus BlueZ
            if let Ok(addrs) = adapter.device_addresses().await {
                for addr in addrs {
                    if let Ok(dev) = adapter.device(addr) {
                        let connected = dev.is_connected().await.unwrap_or(false);
                        let trusted = dev.is_trusted().await.unwrap_or(false);
                        if !connected && !trusted {
                            let _ = adapter.remove_device(addr).await;
                        }
                    }
                }
            }

            // Charakteristik 1: Briefkasten für Paket 2 (Write) - als Fallback
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

            // Charakteristik 2: Ausgabefach für Paket 3 "SSID;PASSWORT;IP" (Read) - als Fallback
            let ack_char = Characteristic {
                uuid: Uuid::from_u128(RUSTLE_ACK_CHAR_UUID),
                read: Some(CharacteristicRead {
                    read: true,
                    fun: Box::new({
                        let creds = creds_clone.clone();
                        move |_req| {
                            let creds = creds.clone();
                            Box::pin(async move {
                                let val = creds
                                    .lock()
                                    .map(|c| c.as_bytes().to_vec())
                                    .unwrap_or_default();
                                Ok(val)
                            })
                        }
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

            // CRITICAL BLE LIMIT CHECK:
            // Maximal 31 Bytes pro Advertising Paket.
            // Wenn Manufacturer Data (0x5255, 25 Bytes) gesetzt ist, DARF KEINE 128-Bit Service UUID
            // (18 Bytes) im selben Paket stehen, da 25 + 18 = 43 > 31 Bytes BlueZ HCI Error 0x12 wirft!
            let mut service_uuids = BTreeSet::new();
            let mut manufacturer_data = BTreeMap::new();
            let mut has_man_data = false;

            if is_go {
                if let Ok(creds) = creds_clone.lock() {
                    if creds.contains(';') {
                        let parts: Vec<&str> = creds.split(';').collect();
                        if parts.len() >= 3 {
                            if let Some(packed) = pack_wifi_credentials(parts[0], parts[1], parts[2]) {
                                println!(
                                    "[LINUX BLE] Packe Zugangsdaten '{}' in 0x5255 Manufacturer Data ({} Bytes)",
                                    creds, packed.len()
                                );
                                manufacturer_data.insert(RUSTLE_COMPANY_ID, packed);
                                has_man_data = true;
                            }
                        }
                    }
                }
            }

            if !has_man_data {
                // Client-Modus: Service UUID ins Advertising
                service_uuids.insert(Uuid::from_u128(RUSTLE_SERVICE_UUID));
            }

            let le_advertisement = Advertisement {
                advertisement_type: bluer::adv::Type::Peripheral,
                service_uuids,
                manufacturer_data,
                local_name: Some(local_name),
                discoverable: Some(true),
                ..Default::default()
            };

            let adv_h = adapter
                .advertise(le_advertisement)
                .await
                .map_err(|e| e.to_string())?;
            println!("[LINUX BLE] Advertisement erfolgreich bei BlueZ registriert!");
            Ok::<_, String>((adv_h, app_h))
        })?;

        self.adv_handle = Some(adv_handle);
        self.app_handle = Some(app_handle);
        Ok(format!(
            "Linux GO bereit ('{}') - Hotspot aktiv & werbung läuft...",
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

            // Smart Early-Exit Scan: Prüft alle 150ms, ob bereits ein Rustle-GO ("R_GO_" oder Manufacturer Data 0x5255)
            // gefunden wurde, und beendet den Scan dann sofort statt volle 4s zu warten!
            let max_steps = ((duration_secs * 1000) / 150).max(4);
            for step in 1..=max_steps {
                tokio::time::sleep(Duration::from_millis(150)).await;
                if step >= 3 {
                    if let Ok(addrs) = adapter.device_addresses().await {
                        let mut found_go = false;
                        for addr in addrs {
                            if let Ok(dev) = adapter.device(addr) {
                                let name = dev.name().await.ok().flatten().unwrap_or_default();
                                let uuids = dev.uuids().await.ok().flatten().unwrap_or_default();
                                let man_data = dev.manufacturer_data().await.ok().flatten().unwrap_or_default();

                                if let Some(bytes) = man_data.get(&RUSTLE_COMPANY_ID) {
                                    if let Some(creds) = unpack_wifi_credentials(bytes) {
                                        cache_go_credentials(&addr.to_string(), creds);
                                        found_go = true;
                                        break;
                                    }
                                }

                                if name.starts_with("R_GO_")
                                    || uuids.contains(&Uuid::from_u128(RUSTLE_SERVICE_UUID))
                                {
                                    found_go = true;
                                    break;
                                }
                            }
                        }
                        if found_go {
                            println!(
                                "[LINUX BLE] Rustle GO bereits nach {}ms gefunden -> Beende Scan vorzeitig!",
                                step * 150
                            );
                            break;
                        }
                    }
                }
            }

            let mut peers = Vec::new();
            let device_addresses = adapter.device_addresses().await.map_err(|e| e.to_string())?;

            for addr in device_addresses {
                if let Ok(dev) = adapter.device(addr) {
                    let addr_type = dev.address_type().await.ok();
                    let name = dev.name().await.ok().flatten().unwrap_or_default();
                    let rssi = dev.rssi().await.ok().flatten().unwrap_or(0);
                    let uuids = dev.uuids().await.ok().flatten().unwrap_or_default();
                    let man_data = dev.manufacturer_data().await.ok().flatten().unwrap_or_default();

                    let mut has_our_man_data = false;
                    if let Some(bytes) = man_data.get(&RUSTLE_COMPANY_ID) {
                        if let Some(creds) = unpack_wifi_credentials(bytes) {
                            println!(
                                "[LINUX BLE] Scan fand Manufacturer Data für {} -> {}",
                                addr, creds
                            );
                            cache_go_credentials(&addr.to_string(), creds);
                            has_our_man_data = true;
                        }
                    }

                    let has_our_uuid = uuids.contains(&Uuid::from_u128(RUSTLE_SERVICE_UUID));
                    let is_rustle_name = name.starts_with("R_GO_") || name.starts_with("R_CL_");

                    if has_our_uuid
                        || has_our_man_data
                        || is_rustle_name
                        || (!name.is_empty() && addr_type != Some(AddressType::BrEdr))
                    {
                        let is_go = has_our_man_data || name.starts_with("R_GO_");
                        let display_name = if !name.is_empty() {
                            name
                        } else if has_our_man_data {
                            "R_GO_Android".into()
                        } else if has_our_uuid {
                            "R_Peer".into()
                        } else {
                            "<Unbekannt>".into()
                        };
                        peers.push(DiscoveredPeer {
                            address: addr.to_string(),
                            name: display_name,
                            is_go,
                            rssi,
                        });
                    }
                }
            }
            // Sortiere GOs nach ganz oben
            peers.sort_by(|a, b| b.is_go.cmp(&a.is_go).then(b.rssi.cmp(&a.rssi)));
            Ok(peers)
        })
    }

    fn send_connection_request(
        &mut self,
        target_address: &str,
        client_name: &str,
    ) -> Result<String, String> {
        // 1. FAST PATH: Connectionless BLE Handshake über Manufacturer Data (0ms!)
        if let Some(cached_creds) = get_cached_go_credentials(target_address) {
            println!(
                "[LINUX BLE] Connectionless Handshake: Zugangsdaten für {} sofort aus BLE Manufacturer Data verfügbar: {} (0ms!)",
                target_address, cached_creds
            );
            return Ok(cached_creds);
        }

        // Warte bis zu 600ms, falls das Scan-Ergebnis mit Manufacturer Data gerade eintrifft
        for w in 1..=6 {
            std::thread::sleep(Duration::from_millis(100));
            if let Some(cached_creds) = get_cached_go_credentials(target_address) {
                println!(
                    "[LINUX BLE] Connectionless Handshake nach {}ms: {}",
                    w * 100, cached_creds
                );
                return Ok(cached_creds);
            }
        }

        println!(
            "[LINUX BLE] Manufacturer Data nicht im Cache -> Starte GATT Fallback zu {}...",
            target_address
        );

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
                        for _ in 0..20 {
                            tokio::time::sleep(Duration::from_millis(120)).await;
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

            // Schnelles Prüfen auf Service Discovery (alle 80ms statt 300ms)
            for _ in 0..40 {
                if device.is_services_resolved().await.unwrap_or(false) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(80)).await;
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

            // 2. Paket 3 sofort lesen (0ms beim ersten Versuch, da GO Hotspot bereits vorbereitet hat!)
            for attempt in 0..80 {
                if attempt > 0 {
                    tokio::time::sleep(Duration::from_millis(150)).await;
                }
                if let Ok(bytes) = ack_char.read().await {
                    if let Ok(creds) = String::from_utf8(bytes) {
                        if creds.contains(';') {
                            println!("[LINUX BLE] Paket 3 empfangen (Versuch {}): {}", attempt, creds);
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

/// Prüft, ob auf Linux ein VPN aktiv ist (Wireguard, OpenVPN, Mullvad, Proton, Nord, etc.)
pub fn check_active_vpn_platform() -> Result<Option<String>, String> {
    Ok(check_active_vpn_linux())
}

pub fn check_active_vpn_linux() -> Option<String> {
    // 1. Scan /sys/class/net auf aktive VPN-Interfaces
    if let Ok(entries) = std::fs::read_dir("/sys/class/net") {
        for entry in entries.flatten() {
            let if_name = entry.file_name().to_string_lossy().to_string();
            let lower = if_name.to_lowercase();
            if lower == "lo"
                || lower.starts_with("wlan")
                || lower.starts_with("wlp")
                || lower.starts_with("eth")
                || lower.starts_with("en")
                || lower.starts_with("p2p")
                || lower.starts_with("br-")
                || lower.starts_with("docker")
                || lower.starts_with("veth")
            {
                continue;
            }
            if lower.starts_with("wg")
                || lower.starts_with("tun")
                || lower.starts_with("tap")
                || lower.starts_with("proton")
                || lower.starts_with("nord")
                || lower.starts_with("ivpn")
                || lower.starts_with("surfshark")
                || lower.starts_with("windscribe")
                || lower.starts_with("xvpn")
            {
                let operstate_path = entry.path().join("operstate");
                let state = std::fs::read_to_string(operstate_path)
                    .unwrap_or_default()
                    .trim()
                    .to_lowercase();
                if state != "down" {
                    return Some(if_name);
                }
            }
        }
    }

    // 2. Fallback: NetworkManager aktive Verbindungen mit VPN-Typ
    if let Ok(out) = std::process::Command::new("nmcli")
        .args(["-t", "-f", "NAME,TYPE", "connection", "show", "--active"])
        .output()
    {
        let stdout = String::from_utf8_lossy(&out.stdout);
        for line in stdout.lines() {
            if let Some((name, ctype)) = line.split_once(':') {
                let ct = ctype.trim().to_lowercase();
                if ct == "vpn" || ct == "wireguard" || ct == "tun" {
                    return Some(name.trim().to_string());
                }
            }
        }
    }
    None
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

/// Öffnet einen TCP-Stream, der explizit an die lokale Subnetz-IP gebunden wird,
/// um VPN-Standard-Routen (0.0.0.0/0 dev wg0) im Linux-Kernel zu umgehen.
pub fn create_connected_wifi_stream(
    target_ip: &str,
    port: u16,
    timeout: Duration,
) -> Result<std::net::TcpStream, String> {
    use socket2::{Domain, Protocol, Socket, Type};
    use std::net::{IpAddr, SocketAddr};

    let clean_ip = target_ip.trim();
    let parsed_ip: Ipv4Addr = clean_ip
        .parse()
        .map_err(|e| format!("Ungültige Ziel-IPv4 '{}': {}", clean_ip, e))?;
    let sock_addr = SocketAddr::new(IpAddr::V4(parsed_ip), port);

    let socket = Socket::new(Domain::IPV4, Type::STREAM, Some(Protocol::TCP))
        .map_err(|e| format!("Konnte Socket nicht erstellen: {}", e))?;

    // Binde an lokale IPv4 im selben Subnetz (z.B. 10.42.0.1 oder 192.168.49.x),
    // um die VPN-Default-Route (0.0.0.0/0 dev wg0) im Linux-Kernel FIB zu umgehen!
    if let Some(local_ip) = find_local_ipv4_in_same_subnet(clean_ip) {
        println!(
            "[LINUX SOCKET] Binde Socket an lokale Subnetz-IP {} (bypasses VPN)",
            local_ip
        );
        let local_sock_addr = SocketAddr::new(IpAddr::V4(local_ip), 0);
        if let Err(e) = socket.bind(&local_sock_addr.into()) {
            println!("[LINUX SOCKET] Warnung beim Binden an lokale IP: {}", e);
        }
    }

    socket
        .connect_timeout(&sock_addr.into(), timeout)
        .map_err(|e| format!("Linux TCP-Connect zu {} fehlgeschlagen: {}", sock_addr, e))?;

    let stream: std::net::TcpStream = socket.into();
    stream.set_read_timeout(Some(timeout)).ok();
    stream.set_write_timeout(Some(timeout)).ok();
    Ok(stream)
}

pub fn send_tcp_packet_platform(
    target_ip: &str,
    port: u16,
    message: &str,
) -> Result<String, String> {
    use std::io::{Read, Write};

    println!("[LINUX TCP] Sende Nachricht an {}:{}", target_ip, port);
    let mut stream = create_connected_wifi_stream(target_ip, port, Duration::from_secs(3))?;

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

pub fn export_file_to_downloads_platform(file_path: &std::path::Path) -> Result<String, String> {
    Ok(file_path.to_string_lossy().to_string())
}
