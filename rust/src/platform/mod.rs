#[cfg(target_os = "android")]
mod android;
#[cfg(target_os = "android")]
pub use android::{AndroidBleManager as PlatformBleManager, WifiDirectManager};

#[cfg(all(target_os = "linux", not(target_os = "android")))]
mod linux;
#[cfg(all(target_os = "linux", not(target_os = "android")))]
pub use linux::{LinuxBleManager as PlatformBleManager, WifiDirectManager};

pub const RUSTLE_SERVICE_UUID: u128 = 0x4fafc201_1fb5_459e_8fcc_c5c9c331914b;
pub const RUSTLE_SYN_CHAR_UUID: u128 = 0xbeb5483e_36e1_4688_b7f5_ea07361b26a8;

pub trait WifiDirectDevice {
    fn start_discovery(&mut self) -> Result<(), String>;
    fn connect_to(&mut self, peer_id: String) -> Result<(), String>;
    fn start_hotspot(&mut self, ssid: String, psk: String) -> Result<String, String>;
    fn connect_to_hotspot(&mut self, ssid: String, password: String) -> Result<String, String>;
}

#[derive(Debug, Clone)]
pub struct DiscoveredPeer {
    pub address: String,
    pub name: String,
    pub is_go: bool,
    pub rssi: i16,
}

pub trait BleDevice {
    /// Phase 1: Startet BLE Advertising + GATT-Server (wenn is_go == true)
    fn start_advertising(&mut self, device_name: &str, is_go: bool) -> Result<String, String>;
    fn stop_advertising(&mut self) -> Result<String, String>;
    fn scan_peers(&mut self, duration_secs: u64) -> Result<Vec<DiscoveredPeer>, String>;

    /// Phase 2 (Client -> GO): Verbindet sich per GATT und schreibt "CONNECT;<client_name>"
    fn send_connection_request(&mut self, target_address: &str, client_name: &str) -> Result<String, String>;

    /// Phase 2 (Auf dem GO): Liest aus, ob eine Verbindungsanfrage eingegangen ist
    fn poll_incoming_request(&mut self) -> Result<Option<String>, String>;
}