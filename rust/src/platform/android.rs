use super::{BleDevice, DiscoveredPeer, WifiDirectDevice};
use jni::objects::{GlobalRef, JClass, JObject, JValue};
use jni::{JNIEnv, JavaVM};
use std::sync::OnceLock;

static JVM: OnceLock<JavaVM> = OnceLock::new();
static BRIDGE_CLASS: OnceLock<GlobalRef> = OnceLock::new();

#[no_mangle]
pub extern "system" fn Java_com_example_rustle_NativeWifiBridge_initJni(
    mut env: JNIEnv,
    class: JClass,
    _context: JObject,
) {
    if let Ok(vm) = env.get_java_vm() {
        let _ = JVM.set(vm);
    }
    if let Ok(global_class) = env.new_global_ref(class) {
        let _ = BRIDGE_CLASS.set(global_class);
    }
}

fn call_kotlin_bridge(method_name: &str, sig: &str, args: &[JValue]) -> Result<String, String> {
    let vm = JVM.get().ok_or("JavaVM nicht initialisiert")?;
    let class_ref = BRIDGE_CLASS.get().ok_or("Bridge-Klasse nicht initialisiert")?;

    let mut env = vm.attach_current_thread().map_err(|e| e.to_string())?;
    let class: &JClass = class_ref.as_obj().into();

    let result = env
        .call_static_method(class, method_name, sig, args)
        .map_err(|e| format!("JNI Fehler bei {}: {}", method_name, e))?;

    let jstr = result.l().map_err(|e| e.to_string())?;
    let rust_str: String = env
        .get_string((&jstr).into())
        .map_err(|e| e.to_string())?
        .into();
    Ok(rust_str)
}

pub fn check_active_vpn_platform() -> Result<Option<String>, String> {
    let res = call_kotlin_bridge("getActiveVpnName", "()Ljava/lang/String;", &[])?;
    if res.is_empty() {
        Ok(None)
    } else {
        Ok(Some(res))
    }
}

pub fn create_connected_wifi_stream(
    target_ip: &str,
    port: u16,
    timeout: std::time::Duration,
) -> Result<std::net::TcpStream, String> {
    use std::os::fd::FromRawFd;

    let vm = JVM.get().ok_or("JavaVM fehlt")?;
    let mut env = vm.attach_current_thread().map_err(|e| e.to_string())?;
    let class_ref = BRIDGE_CLASS.get().ok_or("Bridge-Klasse fehlt")?;
    let class: &JClass = class_ref.as_obj().into();

    let j_ip = env.new_string(target_ip).map_err(|e| e.to_string())?;
    let timeout_ms = timeout.as_millis().min(i32::MAX as u128) as i32;

    let res = env.call_static_method(
        class,
        "openConnectedWifiSocket",
        "(Ljava/lang/String;II)I",
        &[
            JValue::Object(&j_ip),
            JValue::Int(port as i32),
            JValue::Int(timeout_ms),
        ],
    );

    match res {
        Ok(val) => {
            let fd = val.i().map_err(|e| e.to_string())?;
            if fd < 0 {
                return Err(format!("Ungültiger File Descriptor: {}", fd));
            }
            let stream = unsafe { std::net::TcpStream::from_raw_fd(fd) };
            stream.set_read_timeout(Some(timeout)).ok();
            stream.set_write_timeout(Some(timeout)).ok();
            Ok(stream)
        }
        Err(e) => {
            let _ = env.exception_clear();
            Err(format!("Android Wi-Fi Socket Connect fehlgeschlagen: {}", e))
        }
    }
}

pub struct WifiDirectManager;
impl WifiDirectManager { pub fn new() -> Self { Self } }

impl WifiDirectDevice for WifiDirectManager {
    fn start_discovery(&mut self) -> Result<(), String> { Ok(()) }
    fn connect_to(&mut self, _peer_id: String) -> Result<(), String> { Ok(()) }
    fn start_hotspot(&mut self, _ssid: String, _psk: String) -> Result<String, String> {
        call_kotlin_bridge("startLocalHotspot", "()Ljava/lang/String;", &[])
    }
    fn connect_to_hotspot(&mut self, ssid: String, password: String) -> Result<String, String> {
        let vm = JVM.get().ok_or("JavaVM fehlt")?;
        let mut env = vm.attach_current_thread().map_err(|e| e.to_string())?;
        let j_ssid = env.new_string(&ssid).map_err(|e| e.to_string())?;
        let j_psk = env.new_string(&password).map_err(|e| e.to_string())?;
        call_kotlin_bridge(
            "connectToSpecifier",
            "(Ljava/lang/String;Ljava/lang/String;)Ljava/lang/String;",
            &[JValue::Object(&j_ssid), JValue::Object(&j_psk)],
        )
    }
    fn stop_hotspot(&mut self) -> Result<String, String> {
        call_kotlin_bridge("stopWifiDirectAndHotspot", "()Ljava/lang/String;", &[])
    }
}

pub struct AndroidBleManager;
impl AndroidBleManager { pub fn new() -> Self { Self } }

impl BleDevice for AndroidBleManager {
    fn start_advertising(&mut self, device_name: &str, is_go: bool) -> Result<String, String> {
        let vm = JVM.get().ok_or("JavaVM fehlt")?;
        let mut env = vm.attach_current_thread().map_err(|e| e.to_string())?;
        let role_prefix = if is_go { "R_GO_" } else { "R_CL_" };
        let full_name = format!("{}{}", role_prefix, device_name);
        let j_name = env.new_string(&full_name).map_err(|e| e.to_string())?;

        call_kotlin_bridge(
            "startBleAdvertising",
            "(Ljava/lang/String;)Ljava/lang/String;",
            &[JValue::Object(&j_name)],
        )
    }

    fn stop_advertising(&mut self) -> Result<String, String> {
        call_kotlin_bridge("stopBleAdvertising", "()Ljava/lang/String;", &[])
    }

    fn scan_peers(&mut self, duration_secs: u64) -> Result<Vec<DiscoveredPeer>, String> {
        let raw_response = call_kotlin_bridge(
            "scanBlePeers",
            "(I)Ljava/lang/String;",
            &[JValue::Int(duration_secs as i32)],
        )?;

        if raw_response.starts_with("ERR:") {
            return Err(raw_response);
        }

        let mut peers = Vec::new();
        for line in raw_response.lines() {
            let parts: Vec<&str> = line.split('|').collect();
            if parts.len() >= 3 {
                let name = parts[1].to_string();
                let is_go = name.starts_with("R_GO_");
                peers.push(DiscoveredPeer {
                    address: parts[0].to_string(),
                    name,
                    is_go,
                    rssi: parts[2].parse().unwrap_or(0),
                });
            }
        }
        Ok(peers)
    }

    fn send_connection_request(&mut self, target_address: &str, client_name: &str) -> Result<String, String> {
        let vm = JVM.get().ok_or("JavaVM fehlt")?;
        let mut env = vm.attach_current_thread().map_err(|e| e.to_string())?;
        let j_addr = env.new_string(target_address).map_err(|e| e.to_string())?;
        let j_name = env.new_string(client_name).map_err(|e| e.to_string())?;

        let res = call_kotlin_bridge(
            "sendBleConnectionRequest",
            "(Ljava/lang/String;Ljava/lang/String;)Ljava/lang/String;",
            &[JValue::Object(&j_addr), JValue::Object(&j_name)],
        )?;

        if res.starts_with("ERR:") {
            Err(res)
        } else {
            Ok(res)
        }
    }

    fn poll_incoming_request(&mut self) -> Result<Option<String>, String> {
        let res = call_kotlin_bridge("pollIncomingBleRequest", "()Ljava/lang/String;", &[])?;
        if res.is_empty() {
            Ok(None)
        } else {
            Ok(Some(res))
        }
    }

    fn set_hotspot_credentials(&mut self, credentials: &str) -> Result<(), String> {
        let vm = JVM.get().ok_or("JavaVM fehlt")?;
        let mut env = vm.attach_current_thread().map_err(|e| e.to_string())?;
        let j_creds = env.new_string(credentials).map_err(|e| e.to_string())?;

        call_kotlin_bridge(
            "setBleHotspotCredentials",
            "(Ljava/lang/String;)Ljava/lang/String;",
            &[JValue::Object(&j_creds)],
        )?;
        Ok(())
    }
}

pub fn send_tcp_packet_platform(target_ip: &str, port: u16, message: &str) -> Result<String, String> {
    let vm = JVM.get().ok_or("JavaVM fehlt")?;
    let mut env = vm.attach_current_thread().map_err(|e| e.to_string())?;

    let j_ip = env.new_string(target_ip).map_err(|e| e.to_string())?;
    let j_msg = env.new_string(message).map_err(|e| e.to_string())?;

    let res = call_kotlin_bridge(
        "sendTcpPacketOverWifi",
        "(Ljava/lang/String;ILjava/lang/String;)Ljava/lang/String;",
        &[
            JValue::Object(&j_ip),
            JValue::Int(port as i32),
            JValue::Object(&j_msg),
        ],
    )?;

    if res.starts_with("ERR:") {
        Err(res)
    } else {
        Ok(res)
    }
}

pub fn export_file_to_downloads_platform(file_path: &std::path::Path) -> Result<String, String> {
    let vm = JVM.get().ok_or("JavaVM fehlt")?;
    let mut env = vm.attach_current_thread().map_err(|e| e.to_string())?;

    let path_str = file_path.to_string_lossy().to_string();
    let file_name = file_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("received_file.bin")
        .to_string();

    let j_path = env.new_string(&path_str).map_err(|e| e.to_string())?;
    let j_name = env.new_string(&file_name).map_err(|e| e.to_string())?;

    let res = call_kotlin_bridge(
        "exportFileToDownloads",
        "(Ljava/lang/String;Ljava/lang/String;)Ljava/lang/String;",
        &[JValue::Object(&j_path), JValue::Object(&j_name)],
    )?;

    if res.starts_with("ERR:") {
        Err(res)
    } else {
        Ok(res)
    }
}
