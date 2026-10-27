#[cfg(target_os = "android")]
mod android;
#[cfg(target_os = "android")]
pub use android::{
    check_active_vpn_platform, create_connected_wifi_stream, export_file_to_downloads_platform,
    get_active_wifi_channel_info_platform, send_tcp_packet_platform,
    AndroidBleManager as PlatformBleManager, WifiDirectManager,
};

#[cfg(all(target_os = "linux", not(target_os = "android")))]
mod linux;
#[cfg(all(target_os = "linux", not(target_os = "android")))]
pub use linux::{
    check_active_vpn_platform, create_connected_wifi_stream, export_file_to_downloads_platform,
    get_active_wifi_channel_info_platform, send_tcp_packet_platform,
    LinuxBleManager as PlatformBleManager, WifiDirectManager,
};

pub const RUSTLE_SERVICE_UUID: u128 = 0x4fafc201_1fb5_459e_8fcc_c5c9c331914b;
pub const RUSTLE_SYN_CHAR_UUID: u128 = 0xbeb5483e_36e1_4688_b7f5_ea07361b26a8;
// Charakteristik für Paket 3 (SSID;PASSWORT;IP vom GO zum Client)
pub const RUSTLE_ACK_CHAR_UUID: u128 = 0x8260c653_1a54_426b_9e36_e84c238bc669;

pub trait WifiDirectDevice {
    fn start_discovery(&mut self) -> Result<(), String>;
    fn connect_to(&mut self, peer_id: String) -> Result<(), String>;
    /// Startet den Hotspot und gibt exakt "SSID;PASSWORT;IP" zurück
    fn start_hotspot(&mut self, ssid: String, psk: String) -> Result<String, String>;
    fn connect_to_hotspot(&mut self, ssid: String, password: String) -> Result<String, String>;
    /// Beendet den Hotspot / die Wi-Fi Direct Verbindung und stellt den Normalzustand wieder her
    fn stop_hotspot(&mut self) -> Result<String, String>;
}

#[derive(Debug, Clone)]
pub struct DiscoveredPeer {
    pub address: String,
    pub name: String,
    pub is_go: bool,
    pub rssi: i16,
}

pub trait BleDevice {
    fn start_advertising(&mut self, device_name: &str, is_go: bool) -> Result<String, String>;
    fn stop_advertising(&mut self) -> Result<String, String>;
    fn scan_peers(&mut self, duration_secs: u64) -> Result<Vec<DiscoveredPeer>, String>;

    /// Sendet Paket 2 (SYN) und wartet anschließend auf Paket 3 (SSID;PASSWORT;IP)
    fn send_connection_request(&mut self, target_address: &str, client_name: &str) -> Result<String, String>;
    fn poll_incoming_request(&mut self) -> Result<Option<String>, String>;

    /// Hinterlegt Paket 3 ("SSID;PASSWORT;IP") in der Read-Charakteristik des GATT-Servers
    fn set_hotspot_credentials(&mut self, credentials: &str) -> Result<(), String>;
}
