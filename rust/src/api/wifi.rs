use crate::platform::{
    BleDevice, DiscoveredPeer, PlatformBleManager, WifiDirectManager,
};
use std::sync::Mutex;

static WIFI_MANAGER: Mutex<Option<WifiDirectManager>> = Mutex::new(None);
static BLE_MANAGER: Mutex<Option<PlatformBleManager>> = Mutex::new(None);

// Wird beim Start aufgerufen, falls auf Android JNI noch nicht getriggert wurde
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

pub fn ble_send_connect_request(target_address: String, client_name: String) -> Result<String, String> {
    let mut ble = BLE_MANAGER.lock().map_err(|e| e.to_string())?;
    if let Some(ref mut mgr) = *ble {
        mgr.send_connection_request(&target_address, &client_name)
    } else {
        Err("BLE Manager nicht initialisiert".into())
    }
}

pub fn ble_poll_incoming_request() -> Result<Option<String>, String> {
    let mut ble = BLE_MANAGER.lock().map_err(|e| e.to_string())?;
    if let Some(ref mut mgr) = *ble {
        mgr.poll_incoming_request()
    } else {
        Err("BLE Manager nicht initialisiert".into())
    }
}