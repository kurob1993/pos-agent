//! Unduh dan pasang model suara natural Supertonic dari GitHub releases.
//! Dijalankan dari tombol di jendela Pengaturan; progres dibaca UI lewat polling.

use std::{
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Mutex,
    },
    thread,
};

use serde::Serialize;
use tauri::{AppHandle, Manager};

use crate::audio;

pub const MODEL_URL: &str = "https://github.com/k2-fsa/sherpa-onnx/releases/download/tts-models/sherpa-onnx-supertonic-3-tts-int8-2026-05-11.tar.bz2";

/// Nama folder di dalam arsip dan nama folder akhir di direktori konfigurasi.
const ARCHIVE_FOLDER: &str = "sherpa-onnx-supertonic-3-tts-int8-2026-05-11";
const ARCHIVE_NAME: &str = "tts-model.download.tar.bz2";
/// Penanda internal bila unduhan dibatalkan pengguna (bukan galat sesungguhnya).
const CANCELLED: &str = "__dibatalkan";

/// Progres unduhan yang dibaca UI.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    /// idle | downloading | extracting | done | error | cancelled
    pub phase: String,
    pub downloaded: u64,
    pub total: u64,
    pub error: Option<String>,
}

/// Kondisi unduhan yang dipakai bersama oleh perintah Tauri dan thread unduh.
#[derive(Default)]
pub struct DownloadState {
    phase: Mutex<String>,
    downloaded: AtomicU64,
    total: AtomicU64,
    error: Mutex<Option<String>>,
    cancel: AtomicBool,
}

pub fn progress(state: &DownloadState) -> Progress {
    Progress {
        phase: state.phase.lock().unwrap().clone(),
        downloaded: state.downloaded.load(Ordering::SeqCst),
        total: state.total.load(Ordering::SeqCst),
        error: state.error.lock().unwrap().clone(),
    }
}

impl DownloadState {
    /// Minta unduhan yang sedang berjalan agar berhenti secepatnya.
    pub fn request_cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }
}

/// Direktori konfigurasi aplikasi; tempat `tts-model` berada.
fn download_dir(app: &AppHandle) -> PathBuf {
    let dir = app.path().app_config_dir().unwrap_or_else(|_| PathBuf::from("."));
    let _ = fs::create_dir_all(&dir);
    dir
}

/// Mulai unduhan di thread terpisah; kembali segera setelah thread berjalan.
pub fn start_download(app: &AppHandle, state: &DownloadState) -> Result<(), String> {
    {
        let current = state.phase.lock().unwrap().clone();
        if current == "downloading" || current == "extracting" {
            return Err("Unduhan sedang berjalan".into());
        }
    }
    if audio::find_model_dir(&[download_dir(app).join("tts-model")]).is_some() {
        return Err("Model sudah terpasang".into());
    }
    *state.phase.lock().unwrap() = "downloading".into();
    state.downloaded.store(0, Ordering::SeqCst);
    state.total.store(0, Ordering::SeqCst);
    *state.error.lock().unwrap() = None;
    state.cancel.store(false, Ordering::SeqCst);
    let app = app.clone();
    thread::spawn(move || run(&app));
    Ok(())
}

/// Jalankan unduh + ekstrak + pasang, simpan hasilnya di state.
fn run(app: &AppHandle) {
    let state = app.state::<DownloadState>();
    let dir = download_dir(app);
    let result = download_and_install(&dir, &state);
    let mut phase = state.phase.lock().unwrap();
    match result {
        Ok(()) => *phase = "done".into(),
        Err(e) if e == CANCELLED => {
            *phase = "cancelled".into();
            state.error.lock().unwrap().clone_from(&None);
        }
        Err(e) => {
            *phase = "error".into();
            state.error.lock().unwrap().replace(e);
        }
    }
    // bersihkan arsip parsial maupun sisa unduhan yang sudah terpasang
    let _ = fs::remove_file(dir.join(ARCHIVE_NAME));
}

fn download_and_install(dir: &Path, state: &DownloadState) -> Result<(), String> {
    let tar_path = dir.join(ARCHIVE_NAME);

    // unduh dengan progres
    let response =
        ureq::get(MODEL_URL).call().map_err(|e| format!("gagal terhubung: {e}"))?;
    let total = response
        .header("Content-Length")
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0);
    state.total.store(total, Ordering::SeqCst);
    let mut reader = response.into_reader();
    let mut file = File::create(&tar_path).map_err(|e| e.to_string())?;
    let mut buffer = [0u8; 64 * 1024];
    let mut downloaded: u64 = 0;
    loop {
        if state.cancel.load(Ordering::SeqCst) {
            return Err(CANCELLED.into());
        }
        let n = reader.read(&mut buffer).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        file.write_all(&buffer[..n]).map_err(|e| e.to_string())?;
        downloaded += n as u64;
        state.downloaded.store(downloaded, Ordering::SeqCst);
    }
    drop(file);

    // ekstrak arsip tar.bz2 ke direktori konfigurasi
    *state.phase.lock().unwrap() = "extracting".into();
    let bz = bzip2::read::MultiBzDecoder::new(File::open(&tar_path).map_err(|e| e.to_string())?);
    tar::Archive::new(bz)
        .unpack(dir)
        .map_err(|e| format!("gagal mengekstrak: {e}"))?;

    // ganti model lama dengan hasil ekstraksi
    let target = dir.join("tts-model");
    if target.exists() {
        fs::remove_dir_all(&target).map_err(|e| e.to_string())?;
    }
    fs::rename(dir.join(ARCHIVE_FOLDER), &target).map_err(|e| e.to_string())?;

    // pastikan seluruh berkas model utuh
    if audio::find_model_dir(&[target]).is_none() {
        return Err("berkas model tidak lengkap setelah ekstraksi".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Unduhan + ekstraksi nyata end-to-end; jalankan dengan: cargo test --release -- --ignored
    /// Catatan: fase "done" dipasang oleh run() (pembungkus Tauri); fungsi ini hanya sampai pemasangan.
    #[test]
    #[ignore]
    fn unduh_dan_pasang_model_nyata() {
        let dir = std::env::temp_dir().join("layar-tts-model-test");
        let _ = fs::create_dir_all(&dir);
        let state = DownloadState::default();
        download_and_install(&dir, &state).expect("pipeline unduh gagal");
        let p = progress(&state);
        assert!(p.total > 100 * 1024 * 1024, "ukuran unduhan masuk akal");
        assert!(audio::find_model_dir(&[dir.join("tts-model")]).is_some(), "model utuh");
    }
}
