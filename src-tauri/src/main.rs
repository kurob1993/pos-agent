// Layar Pelanggan: aplikasi tray di PC kasir. Halaman kasir (browser apa pun) mengirim alamat layar
// pelanggan lewat WebSocket lokal, lalu aplikasi ini menampilkannya layar penuh di monitor kedua.
// Alamat terakhir disimpan, jadi saat login layar langsung tampil tanpa menunggu kasir.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::{
    fs,
    net::{TcpListener, TcpStream},
    path::PathBuf,
    sync::Mutex,
    thread,
    time::Duration,
};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::{
    menu::{Menu, MenuItem},
    tray::TrayIconBuilder,
    AppHandle, Manager, PhysicalPosition, PhysicalSize, RunEvent, WebviewUrl, WebviewWindow, WebviewWindowBuilder,
};
use tauri_plugin_autostart::ManagerExt;
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons};
use tungstenite::{
    accept_hdr,
    handshake::server::{Request, Response},
    Message,
};

/// Port WebSocket lokal; harus sama dengan resources/js/lib/displayApp.js di POS.
const PORT: u16 = 8183;
const WINDOW: &str = "display";
const SETTINGS: &str = "settings";
const NO_MONITOR: &str = "Monitor kedua tidak terdeteksi. Atur layar ke mode Extend.";

/// Pengaturan tampilan yang diubah lewat jendela Pengaturan.
#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
struct DisplayConfig {
    /// Kunci monitor pilihan pengguna; kosong = otomatis (monitor kedua).
    monitor: Option<String>,
    fullscreen: bool,
    /// Ukuran jendela dalam piksel, dipakai saat tidak layar penuh.
    width: u32,
    height: u32,
}

impl Default for DisplayConfig {
    fn default() -> Self {
        Self { monitor: None, fullscreen: true, width: 1280, height: 720 }
    }
}

#[derive(Default, Serialize, Deserialize)]
struct Config {
    /// Situs POS yang sudah diizinkan pengguna menampilkan layar pelanggan.
    #[serde(default)]
    origins: Vec<String>,
    #[serde(default)]
    last_url: Option<String>,
    #[serde(default)]
    display: DisplayConfig,
}

fn config_path(app: &AppHandle) -> Option<PathBuf> {
    app.path().app_config_dir().ok().map(|dir| dir.join("config.json"))
}

fn save_config(app: &AppHandle, config: &Config) {
    let Some(path) = config_path(app) else { return };
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    let _ = fs::write(path, serde_json::to_vec_pretty(config).unwrap_or_default());
}

fn display_config(app: &AppHandle) -> DisplayConfig {
    app.state::<Mutex<Config>>().lock().unwrap().display.clone()
}

/// Pengenal monitor yang disimpan di config: nama monitor, atau posisinya bila tanpa nama.
fn monitor_key(monitor: &tauri::Monitor) -> String {
    match monitor.name() {
        Some(name) => name.to_string(),
        None => format!("{},{}", monitor.position().x, monitor.position().y),
    }
}

/// Monitor kedua = monitor mana pun yang bukan monitor utama.
fn second_monitor(app: &AppHandle) -> Option<tauri::Monitor> {
    let monitors = app.available_monitors().ok()?;
    let primary = app.primary_monitor().ok().flatten().or_else(|| monitors.first().cloned())?;
    monitors.into_iter().find(|m| m.position() != primary.position())
}

/// Monitor pilihan pengguna; bila belum dipilih atau sedang tidak tersambung, pakai monitor kedua.
fn target_monitor(app: &AppHandle, config: &DisplayConfig) -> Option<tauri::Monitor> {
    let chosen = config.monitor.as_deref().and_then(|key| {
        app.available_monitors().ok()?.into_iter().find(|m| monitor_key(m) == key)
    });
    chosen.or_else(|| second_monitor(app))
}

fn place(window: &WebviewWindow, monitor: &tauri::Monitor, config: &DisplayConfig) -> tauri::Result<()> {
    // Fullscreen mengikuti monitor tempat jendela berada, jadi pindahkan dulu baru perbesar
    window.set_fullscreen(false)?;
    window.set_decorations(!config.fullscreen)?;
    let (origin, screen) = (monitor.position(), monitor.size());
    if config.fullscreen {
        window.set_position(*origin)?;
        window.set_size(*screen)?;
    } else {
        let size = PhysicalSize::new(config.width.min(screen.width), config.height.min(screen.height));
        window.set_size(size)?;
        window.set_position(PhysicalPosition::new(
            origin.x + ((screen.width - size.width) / 2) as i32,
            origin.y + ((screen.height - size.height) / 2) as i32,
        ))?;
    }
    window.show()?;
    if config.fullscreen {
        window.set_fullscreen(true)?;
    }
    Ok(())
}

/// Terapkan ulang pengaturan ke jendela layar pelanggan yang sedang terbuka, tanpa memuat ulang halamannya.
fn reposition(app: &AppHandle) -> Result<(), String> {
    let Some(window) = app.get_webview_window(WINDOW) else { return Ok(()) };
    let config = display_config(app);
    let monitor = target_monitor(app, &config).ok_or(NO_MONITOR)?;
    place(&window, &monitor, &config).map_err(|e| e.to_string())
}

fn show(app: &AppHandle, url: &str) -> Result<(), String> {
    let parsed: tauri::Url = url.parse().map_err(|_| "Alamat layar pelanggan tidak sah".to_string())?;
    let config = display_config(app);
    let monitor = target_monitor(app, &config).ok_or(NO_MONITOR)?;

    let window = match app.get_webview_window(WINDOW) {
        Some(window) => {
            window.navigate(parsed).map_err(|e| e.to_string())?;
            window
        }
        None => WebviewWindowBuilder::new(app, WINDOW, WebviewUrl::External(parsed))
            .title("Layar Pelanggan")
            .decorations(false)
            .skip_taskbar(true)
            .visible(false)
            .build()
            .map_err(|e| e.to_string())?,
    };
    place(&window, &monitor, &config).map_err(|e| e.to_string())
}

fn open_settings(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(SETTINGS) {
        let _ = window.show();
        let _ = window.set_focus();
        return;
    }
    let _ = WebviewWindowBuilder::new(app, SETTINGS, WebviewUrl::App("settings.html".into()))
        .title("Pengaturan Layar Pelanggan")
        .inner_size(440.0, 520.0)
        .resizable(false)
        .build();
}

/// Perintah pengaturan hanya boleh dipanggil dari jendela Pengaturan, bukan dari halaman POS di layar pelanggan.
fn only_settings(window: &WebviewWindow) -> Result<(), String> {
    if window.label() == SETTINGS { Ok(()) } else { Err("Tidak diizinkan".into()) }
}

#[tauri::command]
fn get_settings(window: WebviewWindow, app: AppHandle) -> Result<Value, String> {
    only_settings(&window)?;
    let primary = app.primary_monitor().ok().flatten();
    let monitors: Vec<Value> = app
        .available_monitors()
        .map_err(|e| e.to_string())?
        .iter()
        .map(|m| {
            json!({
                "key": monitor_key(m),
                "width": m.size().width,
                "height": m.size().height,
                "primary": primary.as_ref().is_some_and(|p| p.position() == m.position()),
            })
        })
        .collect();
    Ok(json!({ "monitors": monitors, "settings": display_config(&app) }))
}

#[tauri::command]
fn save_settings(window: WebviewWindow, app: AppHandle, settings: DisplayConfig) -> Result<(), String> {
    only_settings(&window)?;
    let settings = DisplayConfig {
        monitor: settings.monitor.filter(|key| !key.is_empty()),
        width: settings.width.clamp(320, 7680),
        height: settings.height.clamp(240, 4320),
        ..settings
    };
    {
        let state = app.state::<Mutex<Config>>();
        let mut config = state.lock().unwrap();
        config.display = settings;
        save_config(&app, &config);
    }
    reposition(&app)
}

/// Hanya situs yang pernah diizinkan pengguna yang boleh menampilkan sesuatu di layar pelanggan.
fn allowed(app: &AppHandle, origin: &str) -> bool {
    let state = app.state::<Mutex<Config>>();
    if state.lock().unwrap().origins.iter().any(|o| o == origin) {
        return true;
    }
    let ok = app
        .dialog()
        .message(format!("Izinkan {origin} menampilkan layar pelanggan di monitor kedua?"))
        .title("Layar Pelanggan")
        .buttons(MessageDialogButtons::OkCancelCustom("Izinkan".into(), "Tolak".into()))
        .blocking_show();
    if ok {
        let mut config = state.lock().unwrap();
        config.origins.push(origin.to_string());
        save_config(app, &config);
    }
    ok
}

fn respond(app: &AppHandle, origin: &str, request: &Value) -> Value {
    match request["type"].as_str() {
        Some("ping") => json!({ "ok": true, "version": env!("CARGO_PKG_VERSION"), "monitor": target_monitor(app, &display_config(app)).is_some() }),
        Some("open") => {
            let url = request["url"].as_str().unwrap_or_default();
            // Situs hanya boleh membuka halamannya sendiri
            if origin.is_empty() || !url.starts_with(&format!("{origin}/")) {
                return json!({ "ok": false, "error": "Alamat tidak berasal dari situs POS ini" });
            }
            if !allowed(app, origin) {
                return json!({ "ok": false, "error": "Situs ini ditolak di aplikasi Layar Pelanggan" });
            }
            match show(app, url) {
                Ok(()) => {
                    let state = app.state::<Mutex<Config>>();
                    let mut config = state.lock().unwrap();
                    config.last_url = Some(url.to_string());
                    save_config(app, &config);
                    json!({ "ok": true })
                }
                Err(error) => json!({ "ok": false, "error": error }),
            }
        }
        _ => json!({ "ok": false, "error": "Perintah tidak dikenal" }),
    }
}

fn serve(app: AppHandle, stream: TcpStream) {
    let mut origin = String::new();
    let handshake = accept_hdr(stream, |request: &Request, response: Response| {
        origin = request.headers().get("origin").and_then(|v| v.to_str().ok()).unwrap_or_default().to_string();
        Ok(response)
    });
    let Ok(mut socket) = handshake else { return };
    while let Ok(message) = socket.read() {
        if !message.is_text() {
            continue;
        }
        let request = serde_json::from_str(message.to_text().unwrap_or_default()).unwrap_or(Value::Null);
        if socket.send(Message::text(respond(&app, &origin, &request).to_string())).is_err() {
            break;
        }
    }
}

fn main() {
    // Wayland melarang aplikasi menentukan posisi jendelanya; lewat X11 (XWayland) boleh
    #[cfg(target_os = "linux")]
    std::env::set_var("GDK_BACKEND", "x11");

    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![get_settings, save_settings])
        .plugin(tauri_plugin_single_instance::init(|_, _, _| {}))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_autostart::init(tauri_plugin_autostart::MacosLauncher::LaunchAgent, None))
        .setup(|app| {
            let handle = app.handle().clone();
            let config: Config = config_path(&handle)
                .and_then(|path| fs::read(path).ok())
                .and_then(|bytes| serde_json::from_slice(&bytes).ok())
                .unwrap_or_default();
            let last_url = config.last_url.clone();
            app.manage(Mutex::new(config));

            // Build rilis mendaftarkan diri ke autostart supaya layar tampil tiap login
            if !cfg!(debug_assertions) {
                let _ = handle.autolaunch().enable();
            }

            let quit = MenuItem::with_id(app, "quit", "Keluar", true, None::<&str>)?;
            let reshow = MenuItem::with_id(app, "show", "Tampilkan ulang", true, None::<&str>)?;
            let settings = MenuItem::with_id(app, "settings", "Pengaturan…", true, None::<&str>)?;
            let mut tray = TrayIconBuilder::new()
                .tooltip("Layar Pelanggan")
                .menu(&Menu::with_items(app, &[&settings, &reshow, &quit])?)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "quit" => app.exit(0),
                    "settings" => open_settings(app),
                    "show" => {
                        let url = app.state::<Mutex<Config>>().lock().unwrap().last_url.clone();
                        if let Some(url) = url {
                            let _ = show(app, &url);
                        }
                    }
                    _ => {}
                });
            if let Some(icon) = app.default_window_icon() {
                tray = tray.icon(icon.clone());
            }
            tray.build(app)?;

            // Hanya dari PC ini: halaman kasir tersambung ke ws://127.0.0.1:8183
            let listener = TcpListener::bind(("127.0.0.1", PORT))?;
            let server = handle.clone();
            thread::spawn(move || {
                for stream in listener.incoming().flatten() {
                    let app = server.clone();
                    thread::spawn(move || serve(app, stream));
                }
            });

            // Saat login monitor kedua bisa terlambat siap: coba sampai 30 detik
            if let Some(url) = last_url {
                thread::spawn(move || {
                    for _ in 0..15 {
                        if show(&handle, &url).is_ok() {
                            break;
                        }
                        thread::sleep(Duration::from_secs(2));
                    }
                });
            }
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("gagal menjalankan Layar Pelanggan")
        .run(|_, event| {
            // Tetap hidup di tray walau jendela layar pelanggan ditutup; keluar hanya lewat menu tray
            if let RunEvent::ExitRequested { api, code, .. } = event {
                if code.is_none() {
                    api.prevent_exit();
                }
            }
        });
}
