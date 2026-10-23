use super::{BleDevice, DiscoveredPeer, WifiDirectDevice, RUSTLE_SERVICE_UUID, RUSTLE_SYN_CHAR_UUID};
use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use zbus::{Connection, zvariant::Value};
use bluer::adv::{Advertisement, AdvertisementHandle};
use bluer::gatt::local::{
    Application, ApplicationHandle, Characteristic, CharacteristicWrite, CharacteristicWriteMethod,
    Service,
};
use bluer::{Address, Uuid};
use std::str::FromStr;

pub struct WifiDirectManager;

impl WifiDirectManager {
    pub fn new() -> Self {
        Self
    }
}

impl WifiDirectDevice for WifiDirectManager {
    fn start_discovery(&mut self) -> Result<(), String> {
        println!("Linux: Wi-Fi P2P Suche gestartet via wpa_supplicant");
        Ok(())
    }

    fn connect_to(&mut self, _peer_id: String) -> Result<(), String> {
        Ok(())
    }

    fn start_hotspot(&mut self, ssid: String, psk: String) -> Result<String, String> {
        async_std_or_tokio_block(async {
            start_hotspot_internal(&ssid, &psk)
                .await
                .map_err(|e| e.to_string())
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

// 1. Hotspot erstellen (Wieder eingefügt)
async fn start_hotspot_internal(ssid: &str, psk: &str) -> Result<String, Box<dyn std::error::Error>> {
    let connection = Connection::system().await?;
    let mut settings: HashMap<&str, HashMap<&str, Value>> = HashMap::new();

    let mut conn_settings: HashMap<&str, Value> = HashMap::new();
    conn_settings.insert("id", Value::from("Rust_Hotspot"));
    conn_settings.insert("type", Value::from("802-11-wireless"));
    conn_settings.insert("autoconnect", Value::from(false));
    settings.insert("connection", conn_settings);

    let mut wifi_settings: HashMap<&str, Value> = HashMap::new();
    wifi_settings.insert("mode", Value::from("ap"));
    wifi_settings.insert("ssid", Value::from(ssid.as_bytes().to_vec()));
    settings.insert("802-11-wireless", wifi_settings);

    let mut sec_settings: HashMap<&str, Value> = HashMap::new();
    sec_settings.insert("key-mgmt", Value::from("wpa-psk"));
    sec_settings.insert("psk", Value::from(psk));
    settings.insert("802-11-wireless-security", sec_settings);

    let mut ip4_settings: HashMap<&str, Value> = HashMap::new();
    ip4_settings.insert("method", Value::from("shared"));
    settings.insert("ipv4", ip4_settings);

    let proxy = zbus::Proxy::new(
        &connection,
        "org.freedesktop.NetworkManager",
        "/org/freedesktop/NetworkManager/Settings",
        "org.freedesktop.NetworkManager.Settings",
    ).await?;

    let path: zbus::zvariant::OwnedObjectPath = proxy.call("AddConnection", &(settings)).await?;

    let nm_proxy = zbus::Proxy::new(
        &connection,
        "org.freedesktop.NetworkManager",
        "/org/freedesktop/NetworkManager",
        "org.freedesktop.NetworkManager",
    ).await?;

    let active_path: zbus::zvariant::OwnedObjectPath = nm_proxy
        .call(
            "ActivateConnection",
            &(
                &path,
                zbus::zvariant::ObjectPath::from_str_unchecked("/"),
                zbus::zvariant::ObjectPath::from_str_unchecked("/"),
            ),
        )
        .await?;

    Ok(format!("Hotspot aktiv: {:?}", active_path))
}

// 2. Mit Hotspot verbinden
async fn connect_to_hotspot_internal(ssid: &str, password: &str) -> Result<String, Box<dyn std::error::Error>> {
    let connection = Connection::system().await?;
    let mut settings: HashMap<&str, HashMap<&str, Value>> = HashMap::new();

    let mut conn_settings: HashMap<&str, Value> = HashMap::new();
    conn_settings.insert("id", Value::from(ssid));
    conn_settings.insert("type", Value::from("802-11-wireless"));
    settings.insert("connection", conn_settings);

    let mut wifi_settings: HashMap<&str, Value> = HashMap::new();
    wifi_settings.insert("mode", Value::from("infrastructure"));
    wifi_settings.insert("ssid", Value::from(ssid.as_bytes().to_vec()));
    settings.insert("802-11-wireless", wifi_settings);

    let mut sec_settings: HashMap<&str, Value> = HashMap::new();
    sec_settings.insert("key-mgmt", Value::from("wpa-psk"));
    sec_settings.insert("psk", Value::from(password));
    settings.insert("802-11-wireless-security", sec_settings);

    let proxy = zbus::Proxy::new(
        &connection,
        "org.freedesktop.NetworkManager",
        "/org/freedesktop/NetworkManager/Settings",
        "org.freedesktop.NetworkManager.Settings",
    ).await?;

    let path: zbus::zvariant::OwnedObjectPath = proxy.call("AddConnection", &(settings)).await?;

    let nm_proxy = zbus::Proxy::new(
        &connection,
        "org.freedesktop.NetworkManager",
        "/org/freedesktop/NetworkManager",
        "org.freedesktop.NetworkManager",
    ).await?;

    let active_path: zbus::zvariant::OwnedObjectPath = nm_proxy
        .call(
            "ActivateConnection",
            &(
                &path,
                zbus::zvariant::ObjectPath::from_str_unchecked("/"),
                zbus::zvariant::ObjectPath::from_str_unchecked("/"),
            ),
        )
        .await?;

    Ok(format!("Verbindung initiiert: {:?}", active_path))
}

// -----------------------------------
// BLE Manager (Linux via BlueZ / bluer)
// -----------------------------------

pub struct LinuxBleManager {
    rt: tokio::runtime::Runtime,
    adv_handle: Option<AdvertisementHandle>,
    app_handle: Option<ApplicationHandle>,
    incoming_request: Arc<Mutex<Option<String>>>,
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
        }
    }
}

impl BleDevice for LinuxBleManager {
    fn start_advertising(&mut self, device_name: &str, is_go: bool) -> Result<String, String> {
            let role_prefix = if is_go { "R_GO_" } else { "R_CL_" };
            let short_name: String = device_name.chars().take(4).collect();
            let local_name = format!("{}{}", role_prefix, short_name);

            // Klon für die finale Rückgabe anlegen, da das Original in den Async-Block moved
            let local_name_for_return = local_name.clone();

            *self.incoming_request.lock().unwrap() = None;
            let incoming_clone = self.incoming_request.clone();

            let (adv_handle, app_handle) = self.rt.block_on(async move {
                let session = bluer::Session::new().await.map_err(|e| e.to_string())?;
                let adapter = session.default_adapter().await.map_err(|e| e.to_string())?;
                adapter.set_powered(true).await.map_err(|e| e.to_string())?;

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

                let service = Service {
                    uuid: Uuid::from_u128(RUSTLE_SERVICE_UUID),
                    primary: true,
                    characteristics: vec![syn_char],
                    ..Default::default()
                };

                let app = Application {
                    services: vec![service],
                    ..Default::default()
                };

                let app_h = adapter.serve_gatt_application(app).await.map_err(|e| e.to_string())?;

                let mut service_uuids = BTreeSet::new();
                service_uuids.insert(Uuid::from_u128(RUSTLE_SERVICE_UUID));

                let le_advertisement = Advertisement {
                    advertisement_type: bluer::adv::Type::Peripheral,
                    service_uuids,
                    local_name: Some(local_name), // Hier wird das Original reingeschoben
                    ..Default::default()
                };

                let adv_h = adapter.advertise(le_advertisement).await.map_err(|e| e.to_string())?;
                Ok::<_, String>((adv_h, app_h))
            })?;

            self.adv_handle = Some(adv_handle);
            self.app_handle = Some(app_handle);
            // Hier nutzen wir nun den Klon:
            Ok(format!("Linux GO bereit ('{}') - Wartet auf Paket 2...", local_name_for_return))
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

            let _discover = adapter.discover_devices().await.map_err(|e| e.to_string())?;
            tokio::time::sleep(Duration::from_secs(duration_secs)).await;

            let mut peers = Vec::new();
            let device_addresses = adapter.device_addresses().await.map_err(|e| e.to_string())?;

            for addr in device_addresses {
                if let Ok(dev) = adapter.device(addr) {
                    let name = dev.name().await.ok().flatten().unwrap_or_default();
                    let rssi = dev.rssi().await.ok().flatten().unwrap_or(0);
                    let uuids = dev.uuids().await.ok().flatten().unwrap_or_default();

                    let has_our_uuid = uuids.contains(&Uuid::from_u128(RUSTLE_SERVICE_UUID));
                    let is_rustle_name = name.starts_with("R_GO_") || name.starts_with("R_CL_");

                    if has_our_uuid || is_rustle_name || !name.is_empty() {
                        let is_go = name.starts_with("R_GO_");
                        peers.push(DiscoveredPeer {
                            address: addr.to_string(),
                            name: if name.is_empty() { "<Unbekannt>".into() } else { name },
                            is_go,
                            rssi,
                        });
                    }
                }
            }
            Ok(peers)
        })
    }

    fn send_connection_request(&mut self, target_address: &str, client_name: &str) -> Result<String, String> {
        let addr = Address::from_str(target_address).map_err(|e| format!("Ungültige MAC: {}", e))?;
        let payload = format!("CONNECT;{}", client_name);

        self.rt.block_on(async move {
            let session = bluer::Session::new().await.map_err(|e| e.to_string())?;
            let adapter = session.default_adapter().await.map_err(|e| e.to_string())?;
            let device = adapter.device(addr).map_err(|e| e.to_string())?;

            if !device.is_connected().await.unwrap_or(false) {
                device.connect().await.map_err(|e| format!("BLE Connect fehlgeschlagen: {}", e))?;
            }

            // Kurz warten, bis GATT-Services aufgelöst sind
            for _ in 0..10 {
                if device.is_services_resolved().await.unwrap_or(false) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(300)).await;
            }

            let target_service = Uuid::from_u128(RUSTLE_SERVICE_UUID);
            let target_char = Uuid::from_u128(RUSTLE_SYN_CHAR_UUID);

            for service in device.services().await.map_err(|e| e.to_string())? {
                if service.uuid().await.ok() == Some(target_service) {
                    for ch in service.characteristics().await.map_err(|e| e.to_string())? {
                        if ch.uuid().await.ok() == Some(target_char) {
                            ch.write(payload.as_bytes()).await.map_err(|e| format!("GATT Write Fehler: {}", e))?;
                            return Ok(format!("Paket 2 ('{}') erfolgreich an {} gesendet!", payload, target_address));
                        }
                    }
                }
            }

            Err("Rustle GATT-Service/Charakteristik auf Zielgerät nicht gefunden".into())
        })
    }

    fn poll_incoming_request(&mut self) -> Result<Option<String>, String> {
        let mut lock = self.incoming_request.lock().map_err(|e| e.to_string())?;
        Ok(lock.take()) // Gibt die Anfrage zurück und leert das Feld wieder
    }
}