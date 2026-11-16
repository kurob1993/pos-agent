//! Mode tanpa UI (`pos-agent --headless`): hanya WebSocket dan suara, tanpa GTK/jendela/tray,
//! jadi bisa dijalankan dari terminal Linux atau cmd Windows, termasuk di mesin tanpa layar.
//! Memakai config.json yang sama dengan mode biasa; situs harus sudah ada di daftar izin.

use std::{fs, path::PathBuf, process, sync::Arc, thread};

use serde_json::{json, Value};

use crate::{audio, bind, handle_speech, migrate_config_dir, pick, serve, ui_lang, Config};

/// Harus sama dengan `identifier` di tauri.conf.json agar memakai folder konfigurasi yang sama.
const IDENTIFIER: &str = "id.co.krakatau-it.pos-agent";

pub fn run() {
    attach_console();
    let Some(dir) = dirs::config_dir().map(|d| d.join(IDENTIFIER)) else {
        fail("folder konfigurasi tidak ditemukan / config folder not found");
    };
    migrate_config_dir(&dir);
    let config: Config = fs::read(dir.join("config.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    let config = Arc::new(config);

    // Model dicari di folder konfigurasi, lalu di samping program (untuk paket portabel)
    let mut models = vec![dir.join("tts-model")];
    if let Some(exe_dir) = std::env::current_exe().ok().and_then(|p| p.parent().map(PathBuf::from)) {
        models.push(exe_dir.join("tts-model"));
    }

    let listener = match bind(&config.host, config.port) {
        Ok(listener) => listener,
        Err(error) => fail(&format!(
            "tidak bisa memakai {}:{} ({error}); mungkin POS Agent sudah berjalan / cannot use {}:{} ({error}); POS Agent may already be running",
            config.host, config.port, config.host, config.port
        )),
    };
    let _ = listener.set_nonblocking(false);
    println!("POS Agent {} (headless): ws://{}:{}", env!("CARGO_PKG_VERSION"), config.host, config.port);
    println!("config: {}", dir.join("config.json").display());
    match audio::find_model_dir(&models) {
        Some(model) => {
            println!("suara/voice: Supertonic ({})", model.display());
            thread::spawn(move || audio::preload(&model));
        }
        None => println!("suara/voice: TTS OS (model Supertonic tidak ada / not found)"),
    }
    if config.origins.is_empty() {
        println!("peringatan/warning: belum ada situs diizinkan; izinkan dulu lewat mode biasa / no allowed sites yet; allow one in normal mode first");
    }
    println!("Ctrl+C untuk berhenti / to stop");

    for stream in listener.incoming().flatten() {
        let (config, models) = (config.clone(), models.clone());
        thread::spawn(move || serve(stream, |origin, request| respond(&config, &models, origin, request)));
    }
}

fn respond(config: &Config, models: &[PathBuf], origin: &str, request: &Value) -> Value {
    let kind = request["type"].as_str().unwrap_or_default();
    println!("{} {kind}", if origin.is_empty() { "-" } else { origin });
    let lang = ui_lang(config);
    match kind {
        "ping" => json!({ "ok": true, "version": env!("CARGO_PKG_VERSION"), "monitor": false, "headless": true }),
        "speak" | "say" => handle_speech(config, models.to_vec(), origin, request),
        "open" | "close" => json!({ "ok": false, "error": pick(lang, "Mode headless tidak menampilkan layar pelanggan", "Headless mode does not show the customer display") }),
        _ => json!({ "ok": false, "error": pick(lang, "Perintah tidak dikenal", "Unknown command") }),
    }
}

fn fail(message: &str) -> ! {
    eprintln!("POS Agent: {message}");
    process::exit(1);
}

/// Build rilis Windows memakai subsistem GUI (tanpa jendela konsol), jadi tempelkan ke konsol
/// cmd/PowerShell yang memanggil agar keluaran terlihat.
#[cfg(windows)]
fn attach_console() {
    #[link(name = "kernel32")]
    extern "system" {
        fn AttachConsole(process_id: u32) -> i32;
    }
    const ATTACH_PARENT_PROCESS: u32 = u32::MAX;
    unsafe {
        AttachConsole(ATTACH_PARENT_PROCESS);
    }
}

#[cfg(not(windows))]
fn attach_console() {}

#[cfg(test)]
mod tests {
    #[test]
    fn identifier_sama_dengan_tauri_conf() {
        let conf: serde_json::Value = serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        assert_eq!(conf["identifier"], super::IDENTIFIER);
    }
}
