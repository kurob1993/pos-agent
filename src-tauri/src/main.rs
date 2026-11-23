// POS Agent: aplikasi tray di PC kasir. Halaman kasir (browser apa pun) mengirim alamat layar
// pelanggan lewat WebSocket lokal, lalu aplikasi ini menampilkannya layar penuh di monitor kedua.
// Alamat terakhir disimpan (last_url di config.json), jadi saat login layar bisa langsung tampil tanpa menunggu kasir.
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
mod headless;
mod model;
mod voice;

/// Alamat dan port bawaan WebSocket; halaman POS harus memakai port yang sama.
const DEFAULT_HOST: &str = "127.0.0.1";
const DEFAULT_PORT: u16 = 8183;
const WINDOW: &str = "display";
const SETTINGS: &str = "settings";
const PICKER: &str = "picker";

/// Halaman bawaan (ui/index.html) saat belum ada alamat POS atau alamatnya tidak bisa dijangkau.
const DEFAULT_PAGE: &str = if cfg!(windows) { "http://tauri.localhost/" } else { "tauri://localhost/" };

/// Ikuti sumber halaman Tauri: dev server saat pengembangan, aset internal pada build terpasang.
fn app_page(app: &AppHandle, path: &str) -> tauri::Url {
    let base = if cfg!(dev) {
        app.config().build.dev_url.clone()
    } else {
        None
    }.unwrap_or_else(|| DEFAULT_PAGE.parse().unwrap());
    base.join(path).expect("alamat halaman internal tidak sah")
}

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

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
struct AdConfig {
    enabled: bool,
    kind: String,
    content: String,
    idle_seconds: u64,
}

impl Default for AdConfig {
    fn default() -> Self {
        Self { enabled: false, kind: "text".into(), content: String::new(), idle_seconds: 30 }
    }
}

// Waktu request tampilan terakhir dan apakah iklan sedang ditampilkan.
struct DisplayActivity(Mutex<(std::time::Instant, bool)>);

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
struct Config {
    ads: AdConfig,
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
    /// Bahasa suara pengumuman ("id"/"en"); kosong = ikut bahasa tampilan.
    voice_language: Option<String>,
    /// Langkah sintesis suara natural: kecil = jeda lebih singkat, besar = suara lebih halus.
    voice_steps: i32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            ads: AdConfig::default(),
            origins: Vec::new(),
            last_url: None,
            display: DisplayConfig::default(),
            language: None,
            host: DEFAULT_HOST.into(),
            port: DEFAULT_PORT,
            autostart: true,
            voice_language: None,
            voice_steps: 5,
        }
    }
}

/// Semua yang bisa diubah di jendela Pengaturan.
#[derive(Serialize, Deserialize)]
struct Settings {
    ads: AdConfig,
    display: DisplayConfig,
    language: Option<String>,
    host: String,
    port: u16,
    autostart: bool,
    voice_language: Option<String>,
    voice_steps: i32,
}

/// Status listener WebSocket: nomor generasi untuk menghentikan listener lama, dan galat bind terakhir.
#[derive(Default)]
struct Server {
    generation: AtomicU64,
    error: Mutex<Option<String>>,
}

/// Kunci agar pengumuman suara tidak saling tumpang tindih; diambil saat sedang berbicara.
static SPEAKING: Mutex<()> = Mutex::new(());

/// Proses mpv yang sedang memutar iklan video (Linux). Player WebKit tersendat, mpv mulus.
static NATIVE_AD: Mutex<Option<std::process::Child>> = Mutex::new(None);

fn stop_native_ad() {
    if let Some(mut child) = NATIVE_AD.lock().unwrap().take() {
        let _ = child.kill();
        let _ = child.wait();
    }
}

/// Putar iklan video lokal dengan mpv di atas jendela layar pelanggan. false = mpv tidak tersedia.
fn start_native_ad(app: &AppHandle, monitor: &tauri::Monitor, config: &DisplayConfig) -> bool {
    if !cfg!(target_os = "linux") { return false; }
    let ads = app.state::<Mutex<Config>>().lock().unwrap().ads.clone();
    if ads.kind != "video" || !is_local_ad_media(app, &ads.content) { return false; }
    let (origin, screen) = (monitor.position(), monitor.size());
    let mut command = std::process::Command::new("mpv");
    command.args(["--no-config", "--really-quiet", "--no-terminal", "--no-audio", "--loop-file=inf",
        "--ontop", "--no-border", "--no-osc", "--no-input-default-bindings", "--cursor-autohide=always",
        "--title=Iklan", "--force-window=immediate"]);
    if config.fullscreen {
        command.arg("--fs").arg(format!("--geometry=+{}+{}", origin.x, origin.y));
        if let Some(name) = monitor.name() { command.arg(format!("--fs-screen-name={name}")); }
    } else {
        let (w, h) = (config.width.min(screen.width), config.height.min(screen.height));
        command.arg(format!("--geometry={w}x{h}+{}+{}", origin.x + ((screen.width - w) / 2) as i32, origin.y + ((screen.height - h) / 2) as i32));
    }
    match command.arg("--").arg(&ads.content).spawn() {
        Ok(child) => { *NATIVE_AD.lock().unwrap() = Some(child); true }
        Err(_) => false,
    }
}

/// Item menu tray, disimpan agar teksnya bisa diganti saat bahasa berubah.
struct TrayItems {
    settings: MenuItem<tauri::Wry>,
    display: MenuItem<tauri::Wry>,
    quit: MenuItem<tauri::Wry>,
}

/// Aplikasi dulu bernama "Layar Pelanggan": pindahkan folder konfigurasinya (izin situs, pengaturan,
/// model suara ±139 MB) ke folder baru sekali saja, bila folder baru belum ada.
fn migrate_old_config(app: &AppHandle) {
    if let Ok(new) = app.path().app_config_dir() {
        migrate_config_dir(&new);
    }
}

fn migrate_config_dir(new: &std::path::Path) {
    let Some(old) = new.parent().map(|dir| dir.join("id.co.krakatau-it.pos.layar-pelanggan")) else { return };
    if old.is_dir() && !new.exists() {
        let _ = fs::rename(&old, new);
    }
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
    ui_lang(&app.state::<Mutex<Config>>().lock().unwrap())
}

fn ui_lang(config: &Config) -> &'static str {
    match config.language.as_deref() {
        Some("id") => "id",
        Some("en") => "en",
        _ => system_lang(),
    }
}

/// Bahasa suara pengumuman: pilihan sendiri, atau ikut bahasa tampilan.
fn voice_lang_of(config: &Config) -> &'static str {
    match config.voice_language.as_deref() {
        Some("id") => "id",
        Some("en") => "en",
        _ => ui_lang(config),
    }
}

fn pick(lang: &str, id: &str, en: &str) -> String {
    (if lang == "id" { id } else { en }).to_string()
}

fn voice_lang(app: &AppHandle) -> &'static str {
    voice_lang_of(&app.state::<Mutex<Config>>().lock().unwrap())
}

fn tr(app: &AppHandle, id: &str, en: &str) -> String {
    pick(lang(app), id, en)
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

/// Monitor sesuai Pengaturan: monitor pilihan bila diisi (None bila tidak tersambung), selain itu monitor kedua.
fn configured_monitor(app: &AppHandle, config: &DisplayConfig) -> Option<tauri::Monitor> {
    match config.monitor.as_deref() {
        Some(key) => app.available_monitors().ok()?.into_iter().find(|m| monitor_key(m) == key),
        None => second_monitor(app),
    }
}

/// Monitor pilihan pengguna; bila belum dipilih atau sedang tidak tersambung, pakai monitor kedua.
fn target_monitor(app: &AppHandle, config: &DisplayConfig) -> Option<tauri::Monitor> {
    configured_monitor(app, config).or_else(|| second_monitor(app))
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
    display_page(app, url, false)
}

fn display_page(app: &AppHandle, url: &str, is_ad: bool) -> Result<(), String> {
    let parsed: tauri::Url = url
        .parse()
        .map_err(|_| tr(app, "Alamat layar pelanggan tidak sah", "Invalid customer display address"))?;
    let config = display_config(app);
    let monitor = target_monitor(app, &config).ok_or_else(|| no_monitor(app))?;
    stop_native_ad();
    if is_ad { start_native_ad(app, &monitor, &config); }

    let window = match app.get_webview_window(WINDOW) {
        Some(window) => {
            window.navigate(parsed).map_err(|e| e.to_string())?;
            window
        }
        None => WebviewWindowBuilder::new(app, WINDOW, if parsed == app_page(app, "") {
            WebviewUrl::App("index.html".into())
        } else {
            WebviewUrl::External(parsed)
        })
            .title("Layar Pelanggan")
            .decorations(false)
            .skip_taskbar(true)
            .visible(false)
            .build()
            .map_err(|e| e.to_string())?,
    };
    place(&window, &monitor, &config).map_err(|e| e.to_string())?;
    *app.state::<DisplayActivity>().0.lock().unwrap() = (std::time::Instant::now(), is_ad);
    refresh_tray(app);
    Ok(())
}

#[tauri::command]
fn get_ad(window: WebviewWindow, app: AppHandle) -> Result<AdConfig, String> {
    if window.label() != WINDOW { return Err("Forbidden".into()); }
    if NATIVE_AD.lock().unwrap().is_some() {
        // mpv yang memutar videonya; halaman cukup layar hitam di belakangnya
        return Ok(AdConfig { kind: "text".into(), content: String::new(), ..AdConfig::default() });
    }
    Ok(app.state::<Mutex<Config>>().lock().unwrap().ads.clone())
}

#[tauri::command]
async fn pick_ad_media(window: WebviewWindow, app: AppHandle, kind: String) -> Result<Option<String>, String> {
    only_settings(&window)?;
    let extensions: &[&str] = match kind.as_str() {
        "image" => &["png", "jpg", "jpeg", "webp", "gif", "bmp"],
        "video" => &["mp4", "webm", "ogv", "mov", "m4v"],
        _ => return Err("Invalid media type".into()),
    };
    let extensions = extensions.to_vec();
    tauri::async_runtime::spawn_blocking(move || {
        let Some(file) = app.dialog().file().add_filter("Media", &extensions).blocking_pick_file() else {
            return Ok(None);
        };
        let source = file.into_path().map_err(|e| e.to_string())?;
        let extension = source.extension().and_then(|s| s.to_str()).unwrap_or_default().to_lowercase();
        if !extensions.contains(&extension.as_str()) { return Err("Unsupported media format".into()); }
        let directory = app.path().app_data_dir().map_err(|e| e.to_string())?.join("ad-media");
        fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
        let id = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_err(|e| e.to_string())?.as_nanos();
        let destination = directory.join(format!("{id}.{extension}"));
        fs::copy(&source, &destination).map_err(|e| e.to_string())?;
        Ok(Some(destination.to_string_lossy().into_owned()))
    }).await.map_err(|e| e.to_string())?
}

fn is_local_ad_media(app: &AppHandle, content: &str) -> bool {
    let Ok(directory) = app.path().app_data_dir().and_then(|p| p.join("ad-media").canonicalize().map_err(Into::into)) else { return false };
    std::path::Path::new(content).canonicalize().is_ok_and(|p| p.starts_with(directory) && p.is_file())
}

#[tauri::command]
fn show_ad(window: WebviewWindow, app: AppHandle) -> Result<(), String> {
    only_settings(&window)?;
    display_ad(&app)
}

fn display_ad(app: &AppHandle) -> Result<(), String> {
    let ads = app.state::<Mutex<Config>>().lock().unwrap().ads.clone();
    if ads.content.trim().is_empty() {
        return Err(tr(app, "Isi konten iklan terlebih dahulu", "Enter ad content first"));
    }
    let url = if ads.kind == "web" {
        ads.content.parse::<tauri::Url>().map_err(|e| e.to_string())?
    } else {
        app_page(app, "ads.html")
    };
    display_page(app, url.as_str(), true)
}

fn start_ads(app: AppHandle) {
    thread::spawn(move || loop {
        thread::sleep(Duration::from_secs(1));
        let ads = app.state::<Mutex<Config>>().lock().unwrap().ads.clone();
        if app.get_webview_window(WINDOW).is_none() { stop_native_ad(); continue; }
        if !ads.enabled || ads.content.trim().is_empty() { continue; }
        let (last_request, showing_ad) = *app.state::<DisplayActivity>().0.lock().unwrap();
        if showing_ad || last_request.elapsed().as_secs() < ads.idle_seconds { continue; }
        let _ = display_ad(&app);
    });
}

/// Server POS dianggap terjangkau bila menjawab HTTP tanpa galat server (5xx dari proxy = aplikasinya mati).
fn reachable(url: &str) -> bool {
    match ureq::get(url).timeout(Duration::from_secs(5)).call() {
        Ok(_) => true,
        Err(ureq::Error::Status(code, _)) => code < 500,
        Err(_) => false,
    }
}

/// Tampilkan halaman bawaan dulu, lalu pindah ke alamat terakhir begitu situs POS bisa dijangkau
/// (mis. server POS baru siap beberapa saat setelah login). Pengecekan berhenti bila layar sudah
/// tidak menampilkan halaman bawaan: POS mengirim alamat sendiri, atau jendela ditutup.
/// Memanggil jaringan dan menunggu, jadi jalankan di luar thread utama.
fn show_last(app: &AppHandle) -> Result<(), String> {
    let default_page = app_page(app, "");
    show(app, default_page.as_str())?;
    let Some(url) = app.state::<Mutex<Config>>().lock().unwrap().last_url.clone() else { return Ok(()) };
    loop {
        let on_default = app
            .get_webview_window(WINDOW)
            .and_then(|w| w.url().ok())
            .is_some_and(|u| u == default_page);
        if !on_default {
            return Ok(());
        }
        if reachable(&url) {
            return show(app, &url);
        }
        thread::sleep(Duration::from_secs(3));
    }
}

/// Saat aplikasi mulai: tunggu monitor sesuai Pengaturan (bisa terlambat siap saat login, maks. 30 detik).
/// Bila tidak tersambung, layar pelanggan tidak ditampilkan; bila ada, tanya mau tampil di monitor mana.
fn startup(app: AppHandle) {
    thread::spawn(move || {
        for _ in 0..15 {
            if configured_monitor(&app, &display_config(&app)).is_some() {
                open_picker(&app);
                return;
            }
            thread::sleep(Duration::from_secs(2));
        }
    });
}

fn open_picker(app: &AppHandle) {
    let _ = WebviewWindowBuilder::new(app, PICKER, WebviewUrl::App("picker.html".into()))
        .title("POS Agent")
        .inner_size(440.0, 420.0)
        .resizable(false)
        .always_on_top(true)
        .center()
        .build();
}

fn monitors_json(app: &AppHandle) -> Result<Vec<Value>, String> {
    let primary = app.primary_monitor().ok().flatten();
    Ok(app
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
        .collect())
}

fn open_settings(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(SETTINGS) {
        let _ = window.show();
        let _ = window.set_focus();
        return;
    }
    let _ = WebviewWindowBuilder::new(app, SETTINGS, WebviewUrl::App("settings.html".into()))
        .title("POS Agent")
        .inner_size(520.0, 700.0)
        .min_inner_size(420.0, 480.0)
        .build();
}

fn refresh_tray(app: &AppHandle) {
    let items = app.state::<TrayItems>();
    let _ = items.settings.set_text(tr(app, "Pengaturan…", "Settings…"));
    let display_text = if app.get_webview_window(WINDOW).is_some() {
        tr(app, "Tutup layar pelanggan", "Close customer display")
    } else {
        tr(app, "Tampilkan layar pelanggan", "Show customer display")
    };
    let _ = items.display.set_text(display_text);
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

#[tauri::command]
fn check_port(window: WebviewWindow, app: AppHandle, host: String, port: u16) -> Result<bool, String> {
    only_settings(&window)?;
    let host = host.trim();
    if host != "localhost" && host.parse::<IpAddr>().is_err() {
        return Err(tr(
            &app,
            "Host harus berupa alamat IP (mis. 127.0.0.1) atau localhost",
            "Host must be an IP address (e.g. 127.0.0.1) or localhost",
        ));
    }
    if port < 1024 {
        return Err(tr(&app, "Port harus antara 1024 dan 65535", "Port must be between 1024 and 65535"));
    }

    let config = app.state::<Mutex<Config>>();
    let current = config.lock().unwrap();
    if host == current.host && port == current.port && app.state::<Server>().error.lock().unwrap().is_none() {
        return Ok(true);
    }
    drop(current);

    match TcpListener::bind((host, port)) {
        Ok(listener) => {
            drop(listener);
            Ok(true)
        }
        Err(error) if error.kind() == io::ErrorKind::AddrInUse => Ok(false),
        Err(error) => Err(bind_error(&app, host, port, error)),
    }
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
                thread::spawn(move || serve(stream, |origin, request| respond(&app, origin, request)));
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
    let monitors = monitors_json(&app)?;
    let settings = {
        let state = app.state::<Mutex<Config>>();
        let config = state.lock().unwrap();
        Settings {
            ads: config.ads.clone(),
            display: config.display.clone(),
            language: config.language.clone(),
            host: config.host.clone(),
            port: config.port,
            autostart: config.autostart,
            voice_language: config.voice_language.clone(),
            voice_steps: config.voice_steps,
        }
    };
    let listener_error = app.state::<Server>().error.lock().unwrap().clone();
    Ok(json!({
        "monitors": monitors,
        "settings": settings,
        "system_lang": system_lang(),
        "listener_error": listener_error,
        "dev": cfg!(debug_assertions),
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

/// Data untuk dialog pilih monitor saat startup.
#[tauri::command]
fn picker_data(window: WebviewWindow, app: AppHandle) -> Result<Value, String> {
    if window.label() != PICKER {
        return Err("Forbidden".into());
    }
    let selected = configured_monitor(&app, &display_config(&app)).map(|m| monitor_key(&m));
    Ok(json!({ "monitors": monitors_json(&app)?, "selected": selected, "lang": lang(&app) }))
}

/// Pilihan dari dialog startup: tampilkan di monitor `key`, atau tidak sama sekali bila None.
/// Monitor yang berbeda dari Pengaturan disimpan agar perintah POS berikutnya ikut ke monitor itu.
#[tauri::command]
fn pick_monitor(window: WebviewWindow, app: AppHandle, key: Option<String>) -> Result<(), String> {
    if window.label() != PICKER {
        return Err("Forbidden".into());
    }
    let _ = window.close();
    let Some(key) = key else { return Ok(()) };
    let current = configured_monitor(&app, &display_config(&app)).map(|m| monitor_key(&m));
    if current.as_deref() != Some(key.as_str()) {
        let state = app.state::<Mutex<Config>>();
        let mut config = state.lock().unwrap();
        config.display.monitor = Some(key);
        save_config(&app, &config);
    }
    thread::spawn(move || {
        let _ = show_last(&app);
    });
    Ok(())
}

#[tauri::command]
fn save_settings(window: WebviewWindow, app: AppHandle, settings: Settings) -> Result<(), String> {
    only_settings(&window)?;
    if !["text", "image", "video", "web"].contains(&settings.ads.kind.as_str()) || settings.ads.idle_seconds < 1 || settings.ads.idle_seconds > 86400 {
        return Err(tr(&app, "Pengaturan iklan tidak sah (waktu tunggu 1–86400 detik)", "Invalid ad settings (idle time 1–86400 seconds)"));
    }
    if settings.ads.enabled && settings.ads.content.trim().is_empty() {
        return Err(tr(&app, "Isi konten iklan terlebih dahulu", "Enter ad content first"));
    }
    if settings.ads.kind != "text" && !settings.ads.content.trim().is_empty() {
        let remote = settings.ads.content.parse::<tauri::Url>().is_ok_and(|u| matches!(u.scheme(), "http" | "https"));
        let local = matches!(settings.ads.kind.as_str(), "image" | "video") && is_local_ad_media(&app, &settings.ads.content);
        if !remote && !local { return Err(tr(&app, "Pilih file media atau isi URL HTTP/HTTPS yang sah", "Choose a media file or enter a valid HTTP/HTTPS URL")); }
    }
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
        config.ads = settings.ads;
        config.display = display;
        config.language = language;
        config.host = host;
        config.port = settings.port;
        config.autostart = autostart_on;
        config.voice_steps = settings.voice_steps.clamp(2, 10);
        config.voice_language = settings.voice_language.filter(|l| l == "id" || l == "en");
        save_config(&app, &config);
    }
    refresh_tray(&app);
    let showing_ad = app.state::<DisplayActivity>().0.lock().unwrap().1;
    if showing_ad && app.get_webview_window(WINDOW).is_some() {
        let enabled = state.lock().unwrap().ads.enabled;
        if enabled { display_ad(&app)?; }
        else { show(&app, app_page(&app, "").as_str())?; }
    }
    autostart?;
    reposition(&app)
}

/// Status model suara natural untuk tombol di Pengaturan.
#[tauri::command]
fn voice_model_status(window: WebviewWindow, app: AppHandle) -> Result<Value, String> {
    only_settings(&window)?;
    let installed = audio::find_model_dir(&model_dir_candidates(&app)).is_some();
    Ok(json!({ "installed": installed }))
}

/// Mulai unduh model suara natural; berjalan di thread terpisah.
#[tauri::command]
fn download_voice_model(window: WebviewWindow, app: AppHandle) -> Result<(), String> {
    only_settings(&window)?;
    model::start_download(&app, &app.state::<model::DownloadState>())
}

/// Baca progres unduhan model (dipolling oleh UI Pengaturan).
#[tauri::command]
fn voice_download_progress(window: WebviewWindow, app: AppHandle) -> Result<model::Progress, String> {
    only_settings(&window)?;
    Ok(model::progress(&app.state::<model::DownloadState>()))
}

/// Batalkan unduhan model yang sedang berjalan.
#[tauri::command]
fn cancel_voice_download(window: WebviewWindow, app: AppHandle) -> Result<(), String> {
    only_settings(&window)?;
    app.state::<model::DownloadState>().request_cancel();
    Ok(())
}

/// Putar uji suara dari Pengaturan: koin + `text`, atau contoh pengumuman pembayaran bila kosong.
#[tauri::command]
fn test_voice(window: WebviewWindow, app: AppHandle, text: Option<String>) -> Result<(), String> {
    only_settings(&window)?;
    let text = match text.map(|t| t.trim().chars().take(500).collect::<String>()).filter(|t| !t.is_empty()) {
        Some(text) => text,
        None => voice::payment_text(voice_lang(&app), "qris", 25000.0).ok_or_else(|| "kalimat uji tidak sah".to_string())?,
    };
    // thread terpisah agar UI tidak membeku selama sintesis
    thread::spawn(move || speak_text(&app, &text, true));
    Ok(())
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
        .title("POS Agent")
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
                return json!({ "ok": false, "error": tr(app, "Situs ini ditolak di aplikasi POS Agent", "This site was denied in the POS Agent app") });
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
                return not_allowed(lang(app));
            }
            if app.state::<Mutex<Config>>().lock().unwrap().ads.enabled {
                let _ = show(app, app_page(app, "").as_str());
            } else if let Some(window) = app.get_webview_window(WINDOW) {
                let _ = window.close();
            }
            // Alamat dilupakan agar layar tidak muncul lagi saat login berikutnya
            let state = app.state::<Mutex<Config>>();
            let mut config = state.lock().unwrap();
            config.last_url = None;
            save_config(app, &config);
            json!({ "ok": true })
        }
        Some("speak" | "say") => {
            let config = app.state::<Mutex<Config>>().lock().unwrap().clone();
            handle_speech(&config, model_dir_candidates(app), origin, request)
        }
        _ => json!({ "ok": false, "error": tr(app, "Perintah tidak dikenal", "Unknown command") }),
    }
}

fn not_allowed(lang: &str) -> Value {
    json!({ "ok": false, "error": pick(lang, "Situs ini belum diizinkan di aplikasi POS Agent", "This site is not yet allowed in the POS Agent app") })
}

/// Teks yang diucapkan untuk perintah `speak` (pengumuman pembayaran) atau `say` (teks bebas),
/// beserta apakah denting koin diputar di depan. Galat dalam bahasa tampilan.
fn speech_request(config: &Config, request: &Value) -> Result<(String, bool), String> {
    let lang = ui_lang(config);
    if request["type"] == "speak" {
        let amount = request["amount"].as_f64().ok_or_else(|| pick(lang, "Amount harus berupa angka", "Amount must be a number"))?;
        let method = request["method"].as_str().unwrap_or_default();
        let text = voice::payment_text(voice_lang_of(config), method, amount).ok_or_else(|| {
            pick(lang, "Method harus cash, edc, atau qris, dan amount harus angka positif", "Method must be cash, edc, or qris, and amount must be a positive number")
        })?;
        return Ok((text, true));
    }
    let text = request["text"].as_str().unwrap_or_default().trim().to_string();
    if text.is_empty() || text.chars().count() > 500 {
        return Err(pick(lang, "Text wajib diisi, maksimal 500 karakter", "Text is required, at most 500 characters"));
    }
    Ok((text, request["coin"].as_bool().unwrap_or(false)))
}

/// Perintah `speak`/`say`: periksa izin dan isi, lalu ucapkan di thread terpisah agar POS tidak menunggu.
fn handle_speech(config: &Config, models: Vec<PathBuf>, origin: &str, request: &Value) -> Value {
    if !config.origins.iter().any(|o| o == origin) {
        return not_allowed(ui_lang(config));
    }
    match speech_request(config, request) {
        Ok((text, coin)) => {
            let (language, steps) = (voice_lang_of(config), config.voice_steps);
            thread::spawn(move || speak(&models, language, steps, &text, coin));
            json!({ "ok": true })
        }
        Err(error) => json!({ "ok": false, "error": error }),
    }
}

fn speak_text(app: &AppHandle, text: &str, coin: bool) {
    let (language, steps) = {
        let config = app.state::<Mutex<Config>>();
        let config = config.lock().unwrap();
        (voice_lang_of(&config), config.voice_steps)
    };
    speak(&model_dir_candidates(app), language, steps, text, coin);
}

/// Ucapkan pengumuman: denting koin + suara natural Supertonic bila model tersedia,
/// selain itu jatuh ke mesin TTS OS.
fn speak(models: &[PathBuf], language: &str, steps: i32, text: &str, coin: bool) {
    // Antrekan bila ada pengumuman lain sedang berbicara
    let _guard = SPEAKING.lock().unwrap_or_else(|e| e.into_inner());
    let text = &voice::normalize_text(language, text);
    if let Some(model) = audio::find_model_dir(models) {
        if audio::speak_supertonic(&model, coin.then_some(COIN), text, steps.clamp(2, 10), language) {
            return;
        }
        eprintln!("POS Agent: gagal memuat model Supertonic, pakai TTS OS");
    }
    if (!coin || audio::play_coin(COIN).is_ok()) && audio::speak_os(language, text) {
        return;
    }
    eprintln!("POS Agent: tidak ada mesin suara yang bisa dipakai");
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

/// Satu koneksi WebSocket: setiap pesan teks JSON dijawab `respond(origin, request)`.
fn serve(stream: TcpStream, respond: impl Fn(&str, &Value) -> Value) {
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
        if socket.send(Message::text(respond(&origin, &request).to_string())).is_err() {
            break;
        }
    }
}

fn main() {
    if std::env::args().any(|arg| arg == "--headless") {
        headless::run();
        return;
    }
    // Wayland melarang aplikasi menentukan posisi jendelanya; lewat X11 (XWayland) boleh
    #[cfg(target_os = "linux")]
    {
        std::env::set_var("GDK_BACKEND", "x11");
        // Biarkan WebKit memilih renderer untuk scaling dan komposisi.
        // Pilihan decoder software di bawah hanya berlaku untuk H.264.
        let ranks = std::env::var("GST_PLUGIN_FEATURE_RANK").unwrap_or_default();
        if !ranks.split(',').any(|entry| entry.trim().starts_with("avdec_h264:")) {
            let ranks = if ranks.is_empty() { "avdec_h264:512".into() } else { format!("{ranks},avdec_h264:512") };
            std::env::set_var("GST_PLUGIN_FEATURE_RANK", ranks);
        }
    }

    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            get_settings,
            get_ad,
            pick_ad_media,
            show_ad,
            check_port,
            save_settings,
            voice_model_status,
            download_voice_model,
            voice_download_progress,
            cancel_voice_download,
            test_voice,
            picker_data,
            pick_monitor
        ])
        .plugin(tauri_plugin_single_instance::init(|_, _, _| {}))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_autostart::init(tauri_plugin_autostart::MacosLauncher::LaunchAgent, None))
        .setup(|app| {
            let handle = app.handle().clone();
            migrate_old_config(&handle);
            let config: Config = config_path(&handle)
                .and_then(|path| fs::read(path).ok())
                .and_then(|bytes| serde_json::from_slice(&bytes).ok())
                .unwrap_or_default();
            let (host, port, autostart) = (config.host.clone(), config.port, config.autostart);
            app.manage(Mutex::new(config));
            app.manage(Server::default());
            app.manage(DisplayActivity(Mutex::new((std::time::Instant::now(), false))));
            app.manage(model::DownloadState::default());

            let _ = apply_autostart(&handle, autostart);

            let quit = MenuItem::with_id(app, "quit", tr(&handle, "Keluar", "Quit"), true, None::<&str>)?;
            let display = MenuItem::with_id(app, "display", tr(&handle, "Tampilkan layar pelanggan", "Show customer display"), true, None::<&str>)?;
            let settings = MenuItem::with_id(app, "settings", tr(&handle, "Pengaturan…", "Settings…"), true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&settings, &display, &quit])?;
            app.manage(TrayItems { settings, display, quit });

            let mut tray = TrayIconBuilder::new()
                .tooltip("POS Agent")
                .menu(&menu)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "quit" => app.exit(0),
                    "settings" => open_settings(app),
                    "display" => {
                        if let Some(window) = app.get_webview_window(WINDOW) {
                            let _ = window.close();
                            refresh_tray(app);
                        } else {
                            let app = app.clone();
                            thread::spawn(move || {
                                let _ = show_last(&app);
                                refresh_tray(&app);
                            });
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

            // Muat model suara di latar agar pengumuman pertama tidak menunggu
            if let Some(model) = audio::find_model_dir(&model_dir_candidates(&handle)) {
                thread::spawn(move || audio::preload(&model));
            }
            start_ads(handle.clone());
            startup(handle);
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("gagal menjalankan POS Agent")
        .run(|app, event| match event {
            // Tetap hidup di tray walau jendela layar pelanggan ditutup; keluar hanya lewat menu tray
            RunEvent::ExitRequested { api, code: None, .. } => api.prevent_exit(),
            // Aplikasi ditutup: lupakan alamat terakhir, jadi start berikutnya menampilkan halaman bawaan
            RunEvent::Exit => {
                stop_native_ad();
                let state = app.state::<Mutex<Config>>();
                let mut config = state.lock().unwrap();
                config.last_url = None;
                save_config(app, &config);
            }
            _ => {}
        });
}
