// Layar Pelanggan: aplikasi tray di PC kasir. Halaman kasir (browser apa pun) mengirim alamat layar
// pelanggan lewat WebSocket lokal, lalu aplikasi ini menampilkannya layar penuh di monitor kedua.
// Alamat terakhir disimpan, jadi saat login layar langsung tampil tanpa menunggu kasir.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::{
    fmt::Display,
    fs, io,
    net::{IpAddr, TcpListener, TcpStream},
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, Ordering},
        Mutex,
    },
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

mod audio;
mod voice;

/// Alamat dan port bawaan WebSocket; halaman POS harus memakai port yang sama.
const DEFAULT_HOST: &str = "127.0.0.1";
const DEFAULT_PORT: u16 = 8183;
const WINDOW: &str = "display";
const SETTINGS: &str = "settings";

/// Efek suara denting koin yang diputar sebelum setiap pengumuman pembayaran.
const COIN: &[u8] = include_bytes!("../assets/coin.wav");

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

#[derive(Serialize, Deserialize)]
#[serde(default)]
struct Config {
    /// Situs POS yang sudah diizinkan pengguna menampilkan layar pelanggan.
    origins: Vec<String>,
    last_url: Option<String>,
    display: DisplayConfig,
    /// "id" atau "en"; kosong = ikut bahasa sistem.
    language: Option<String>,
    /// Alamat tempat WebSocket mendengarkan.
    host: String,
    port: u16,
    /// Jalankan aplikasi otomatis saat login.
    autostart: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            origins: Vec::new(),
            last_url: None,
            display: DisplayConfig::default(),
            language: None,
            host: DEFAULT_HOST.into(),
            port: DEFAULT_PORT,
            autostart: true,
        }
    }
}

/// Semua yang bisa diubah di jendela Pengaturan.
#[derive(Serialize, Deserialize)]
struct Settings {
    display: DisplayConfig,
    language: Option<String>,
    host: String,
    port: u16,
    autostart: bool,
}

/// Status listener WebSocket: nomor generasi untuk menghentikan listener lama, dan galat bind terakhir.
#[derive(Default)]
struct Server {
    generation: AtomicU64,
    error: Mutex<Option<String>>,
    /// Kunci agar pengumuman suara tidak saling tumpang tindih; diambil saat sedang berbicara.
    speaking: Mutex<()>,
}

/// Item menu tray, disimpan agar teksnya bisa diganti saat bahasa berubah.
struct TrayItems {
    settings: MenuItem<tauri::Wry>,
    show: MenuItem<tauri::Wry>,
    quit: MenuItem<tauri::Wry>,
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

fn system_lang() -> &'static str {
    match sys_locale::get_locale() {
        Some(locale) if !locale.to_lowercase().starts_with("id") => "en",
        _ => "id",
    }
}

/// Bahasa yang dipakai: pilihan pengguna, atau bahasa sistem.
fn lang(app: &AppHandle) -> &'static str {
    let language = app.state::<Mutex<Config>>().lock().unwrap().language.clone();
    match language.as_deref() {
        Some("id") => "id",
        Some("en") => "en",
        _ => system_lang(),
    }
}

fn tr(app: &AppHandle, id: &str, en: &str) -> String {
    (if lang(app) == "id" { id } else { en }).to_string()
}

fn no_monitor(app: &AppHandle) -> String {
    tr(
        app,
        "Monitor kedua tidak terdeteksi. Atur layar ke mode Extend.",
        "Second monitor not detected. Set the displays to Extend mode.",
    )
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
    let monitor = target_monitor(app, &config).ok_or_else(|| no_monitor(app))?;
    place(&window, &monitor, &config).map_err(|e| e.to_string())
}

fn show(app: &AppHandle, url: &str) -> Result<(), String> {
    let parsed: tauri::Url = url
        .parse()
        .map_err(|_| tr(app, "Alamat layar pelanggan tidak sah", "Invalid customer display address"))?;
    let config = display_config(app);
    let monitor = target_monitor(app, &config).ok_or_else(|| no_monitor(app))?;

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
        .title("Layar Pelanggan")
        .inner_size(520.0, 700.0)
        .min_inner_size(420.0, 480.0)
        .build();
}

fn refresh_tray(app: &AppHandle) {
    let items = app.state::<TrayItems>();
    let _ = items.settings.set_text(tr(app, "Pengaturan…", "Settings…"));
    let _ = items.show.set_text(tr(app, "Tampilkan ulang", "Show again"));
    let _ = items.quit.set_text(tr(app, "Keluar", "Quit"));
}

/// Mendaftarkan atau mencabut autostart. Build debug tidak menyentuhnya agar program dev tidak ikut terdaftar.
fn apply_autostart(app: &AppHandle, enabled: bool) -> Result<(), String> {
    if cfg!(debug_assertions) {
        return Ok(());
    }
    let manager = app.autolaunch();
    // enable() selalu dipanggil agar path program yang dipindahkan ikut diperbarui
    let result = if enabled {
        manager.enable()
    } else if manager.is_enabled().unwrap_or(false) {
        manager.disable()
    } else {
        Ok(())
    };
    result.map_err(|e| e.to_string())
}

fn bind_error(app: &AppHandle, host: &str, port: u16, error: impl Display) -> String {
    if lang(app) == "id" {
        format!("Tidak bisa memakai {host}:{port} ({error}). Ganti host atau port di Pengaturan.")
    } else {
        format!("Cannot use {host}:{port} ({error}). Change the host or port in Settings.")
    }
}

fn bind(host: &str, port: u16) -> io::Result<TcpListener> {
    let listener = TcpListener::bind((host, port))?;
    listener.set_nonblocking(true)?;
    Ok(listener)
}

/// Jalankan loop accept untuk listener; berhenti sendiri bila nomor generasi sudah berganti.
fn spawn_listener(app: &AppHandle, listener: TcpListener) {
    let generation = app.state::<Server>().generation.fetch_add(1, Ordering::SeqCst) + 1;
    let app = app.clone();
    thread::spawn(move || loop {
        if app.state::<Server>().generation.load(Ordering::SeqCst) != generation {
            break;
        }
        match listener.accept() {
            Ok((stream, _)) => {
                let _ = stream.set_nonblocking(false);
                let app = app.clone();
                thread::spawn(move || serve(app, stream));
            }
            Err(_) => thread::sleep(Duration::from_millis(100)),
        }
    });
}

/// Hentikan listener lama lalu dengarkan di host:port baru; galat disimpan agar tampil di Pengaturan.
fn restart_listener(app: &AppHandle, host: &str, port: u16) -> Result<(), String> {
    let server = app.state::<Server>();
    server.generation.fetch_add(1, Ordering::SeqCst);
    // Beri waktu thread lama keluar supaya socketnya terlepas sebelum bind ulang
    thread::sleep(Duration::from_millis(300));
    let result = bind(host, port).map_err(|e| bind_error(app, host, port, e));
    *server.error.lock().unwrap() = result.as_ref().err().cloned();
    result.map(|listener| spawn_listener(app, listener))
}

/// Perintah pengaturan hanya boleh dipanggil dari jendela Pengaturan, bukan dari halaman POS di layar pelanggan.
fn only_settings(window: &WebviewWindow) -> Result<(), String> {
    if window.label() == SETTINGS { Ok(()) } else { Err("Forbidden".into()) }
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
    let settings = {
        let state = app.state::<Mutex<Config>>();
        let config = state.lock().unwrap();
        Settings {
            display: config.display.clone(),
            language: config.language.clone(),
            host: config.host.clone(),
            port: config.port,
            autostart: config.autostart,
        }
    };
    let listener_error = app.state::<Server>().error.lock().unwrap().clone();
    Ok(json!({
        "monitors": monitors,
        "settings": settings,
        "system_lang": system_lang(),
        "listener_error": listener_error,
        "dev": cfg!(debug_assertions),
    }))
}

#[tauri::command]
fn save_settings(window: WebviewWindow, app: AppHandle, settings: Settings) -> Result<(), String> {
    only_settings(&window)?;
    let host = settings.host.trim().to_string();
    if host != "localhost" && host.parse::<IpAddr>().is_err() {
        return Err(tr(
            &app,
            "Host harus berupa alamat IP (mis. 127.0.0.1) atau localhost",
            "Host must be an IP address (e.g. 127.0.0.1) or localhost",
        ));
    }
    if settings.port < 1024 {
        return Err(tr(&app, "Port harus antara 1024 dan 65535", "Port must be between 1024 and 65535"));
    }
    let language = settings.language.filter(|l| l == "id" || l == "en");
    let display = DisplayConfig {
        monitor: settings.display.monitor.filter(|key| !key.is_empty()),
        width: settings.display.width.clamp(320, 7680),
        height: settings.display.height.clamp(240, 4320),
        ..settings.display
    };

    let state = app.state::<Mutex<Config>>();
    let (old_host, old_port) = {
        let config = state.lock().unwrap();
        (config.host.clone(), config.port)
    };
    let listener_down = app.state::<Server>().error.lock().unwrap().is_some();
    if host != old_host || settings.port != old_port || listener_down {
        if let Err(error) = restart_listener(&app, &host, settings.port) {
            // Kembalikan listener lama supaya POS tidak terputus karena salah ketik
            let _ = restart_listener(&app, &old_host, old_port);
            return Err(error);
        }
    }

    let autostart = apply_autostart(&app, settings.autostart);
    let autostart_on = if autostart.is_ok() {
        settings.autostart
    } else {
        app.autolaunch().is_enabled().unwrap_or(false)
    };
    {
        let mut config = state.lock().unwrap();
        config.display = display;
        config.language = language;
        config.host = host;
        config.port = settings.port;
        config.autostart = autostart_on;
        save_config(&app, &config);
    }
    refresh_tray(&app);
    autostart?;
    reposition(&app)
}

fn is_allowed(app: &AppHandle, origin: &str) -> bool {
    app.state::<Mutex<Config>>().lock().unwrap().origins.iter().any(|o| o == origin)
}

/// Hanya situs yang pernah diizinkan pengguna yang boleh menampilkan sesuatu di layar pelanggan.
fn allowed(app: &AppHandle, origin: &str) -> bool {
    if is_allowed(app, origin) {
        return true;
    }
    let state = app.state::<Mutex<Config>>();
    let ok = app
        .dialog()
        .message(tr(
            app,
            &format!("Izinkan {origin} menampilkan layar pelanggan di monitor kedua?"),
            &format!("Allow {origin} to show the customer display on the second monitor?"),
        ))
        .title("Layar Pelanggan")
        .buttons(MessageDialogButtons::OkCancelCustom(tr(app, "Izinkan", "Allow"), tr(app, "Tolak", "Deny")))
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
                return json!({ "ok": false, "error": tr(app, "Alamat tidak berasal dari situs POS ini", "Address does not belong to this POS site") });
            }
            if !allowed(app, origin) {
                return json!({ "ok": false, "error": tr(app, "Situs ini ditolak di aplikasi Layar Pelanggan", "This site was denied in the Customer Display app") });
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
        Some("close") => {
            // Menutup tidak boleh memicu dialog izin: hanya situs yang sudah diizinkan
            if !is_allowed(app, origin) {
                return json!({ "ok": false, "error": tr(app, "Situs ini belum diizinkan di aplikasi Layar Pelanggan", "This site is not yet allowed in the Customer Display app") });
            }
            if let Some(window) = app.get_webview_window(WINDOW) {
                let _ = window.close();
            }
            // Alamat dilupakan agar layar tidak muncul lagi saat login berikutnya
            let state = app.state::<Mutex<Config>>();
            let mut config = state.lock().unwrap();
            config.last_url = None;
            save_config(app, &config);
            json!({ "ok": true })
        }
        Some("speak") => speak_payment(app, origin, request),
        _ => json!({ "ok": false, "error": tr(app, "Perintah tidak dikenal", "Unknown command") }),
    }
}

/// Ucapkan pengumuman pembayaran berhasil; suara diputar thread terpisah agar POS tidak menunggu.
fn speak_payment(app: &AppHandle, origin: &str, request: &Value) -> Value {
    if !is_allowed(app, origin) {
        return json!({ "ok": false, "error": tr(app, "Situs ini belum diizinkan di aplikasi Layar Pelanggan", "This site is not yet allowed in the Customer Display app") });
    }
    let Some(amount) = request["amount"].as_f64() else {
        return json!({ "ok": false, "error": tr(app, "Amount harus berupa angka", "Amount must be a number") });
    };
    let method = request["method"].as_str().unwrap_or_default();
    let language = lang(app);
    let Some(text) = voice::payment_text(language, method, amount) else {
        return json!({ "ok": false, "error": tr(app, "Method harus cash, edc, atau qris, dan amount harus angka positif", "Method must be cash, edc, or qris, and amount must be a positive number") });
    };
    let app = app.clone();
    thread::spawn(move || speak_text(&app, &text));
    json!({ "ok": true })
}

/// Ucapkan pengumuman: denting koin + suara natural Supertonic bila model tersedia,
/// selain itu jatuh ke mesin TTS OS.
fn speak_text(app: &AppHandle, text: &str) {
    // Antrekan bila ada pengumuman lain sedang berbicara
    let server = app.state::<Server>();
    let _guard = server.speaking.lock().unwrap();
    let language = lang(app);
    if let Some(model) = audio::find_model_dir(&model_dir_candidates(app)) {
        if audio::speak_supertonic(&model, COIN, text) {
            return;
        }
        eprintln!("Layar Pelanggan: gagal memuat model Supertonic, pakai TTS OS");
    }
    if audio::play_coin(COIN).is_ok() && audio::speak_os(language, text) {
        return;
    }
    eprintln!("Layar Pelanggan: tidak ada mesin suara yang bisa dipakai");
}

/// Lokasi model Supertonic yang dicari: folder konfigurasi lalu folder resource aplikasi.
fn model_dir_candidates(app: &AppHandle) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(dir) = app.path().app_config_dir().ok() {
        dirs.push(dir.join("tts-model"));
    }
    if let Ok(dir) = app.path().resource_dir() {
        dirs.push(dir.join("tts-model"));
    }
    dirs
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
            let (host, port, autostart) = (config.host.clone(), config.port, config.autostart);
            app.manage(Mutex::new(config));
            app.manage(Server::default());

            let _ = apply_autostart(&handle, autostart);

            let quit = MenuItem::with_id(app, "quit", tr(&handle, "Keluar", "Quit"), true, None::<&str>)?;
            let reshow = MenuItem::with_id(app, "show", tr(&handle, "Tampilkan ulang", "Show again"), true, None::<&str>)?;
            let settings = MenuItem::with_id(app, "settings", tr(&handle, "Pengaturan…", "Settings…"), true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&settings, &reshow, &quit])?;
            app.manage(TrayItems { settings, show: reshow, quit });

            let mut tray = TrayIconBuilder::new()
                .tooltip("Layar Pelanggan")
                .menu(&menu)
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

            // Port bisa sedang dipakai program lain: jangan crash, buka Pengaturan agar bisa diganti
            match bind(&host, port) {
                Ok(listener) => spawn_listener(&handle, listener),
                Err(error) => {
                    *handle.state::<Server>().error.lock().unwrap() = Some(bind_error(&handle, &host, port, error));
                    open_settings(&handle);
                }
            }

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
