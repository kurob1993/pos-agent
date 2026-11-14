//! Mesin audio pengumuman pembayaran: denting koin lalu ucapan.
//! Ucapan memakai suara natural Supertonic (sherpa-onnx) bila model tersedia,
//! selain itu jatuh ke mesin TTS OS lewat crate `tts`.

use std::{
    collections::HashMap,
    io::Cursor,
    num::{NonZeroU16, NonZeroU32},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, OnceLock,
    },
};

use serde_json::json;

/// Berkas model Supertonic yang wajib ada dalam satu direktori.
const MODEL_FILES: [&str; 7] = [
    "duration_predictor.int8.onnx",
    "text_encoder.int8.onnx",
    "vector_estimator.int8.onnx",
    "vocoder.int8.onnx",
    "tts.json",
    "unicode_indexer.bin",
    "voice.bin",
];

/// Cari direktori model Supertonic di antara kandidat path.
pub fn find_model_dir(candidates: &[PathBuf]) -> Option<PathBuf> {
    candidates
        .iter()
        .find(|d| MODEL_FILES.iter().all(|f| d.join(f).exists()))
        .cloned()
}

/// Engine Supertonic dimuat sekali (model ±140MB) lalu dipakai ulang semua pengumuman.
static ENGINE: OnceLock<Mutex<Option<sherpa_onnx::OfflineTts>>> = OnceLock::new();

/// Jalankan `f` dengan engine; muat model bila belum ada. `None` bila gagal (dicoba lagi nanti).
fn with_engine<T>(model_dir: &Path, f: impl FnOnce(&sherpa_onnx::OfflineTts) -> T) -> Option<T> {
    let cell = ENGINE.get_or_init(|| Mutex::new(None));
    let mut engine = cell.lock().unwrap();
    if engine.is_none() {
        let file = |name: &str| model_dir.join(name).display().to_string();
        let config = sherpa_onnx::OfflineTtsConfig {
            model: sherpa_onnx::OfflineTtsModelConfig {
                supertonic: sherpa_onnx::OfflineTtsSupertonicModelConfig {
                    duration_predictor: Some(file("duration_predictor.int8.onnx")),
                    text_encoder: Some(file("text_encoder.int8.onnx")),
                    vector_estimator: Some(file("vector_estimator.int8.onnx")),
                    vocoder: Some(file("vocoder.int8.onnx")),
                    tts_json: Some(file("tts.json")),
                    unicode_indexer: Some(file("unicode_indexer.bin")),
                    voice_style: Some(file("voice.bin")),
                    ..Default::default()
                },
                num_threads: 2,
                debug: false,
                ..Default::default()
            },
            ..Default::default()
        };
        *engine = sherpa_onnx::OfflineTts::create(&config);
    }
    let tts = engine.as_ref()?;
    Some(f(tts))
}

/// Sintesis kalimat jadi sampel mono + sample rate.
fn synthesize(tts: &sherpa_onnx::OfflineTts, text: &str) -> (Vec<f32>, i32) {
    let mut extra = HashMap::new();
    extra.insert("lang".to_string(), json!("id"));
    let gen = sherpa_onnx::GenerationConfig {
        sid: 0,
        num_steps: 8,
        speed: 1.0,
        extra: Some(extra),
        ..Default::default()
    };
    let audio = tts
        .generate_with_config(text, &gen, None::<fn(&[f32], f32) -> bool>)
        .expect("sintesis Supertonic gagal");
    (audio.samples().to_vec(), audio.sample_rate())
}

/// Ucapkan `text` dengan denting koin di depan memakai Supertonic.
/// Kembalikan false bila model atau engine tidak tersedia (penelepon boleh fallback).
pub fn speak_supertonic(model_dir: &Path, coin: &[u8], text: &str) -> bool {
    let Some((speech, rate)) = with_engine(model_dir, |tts| synthesize(tts, text)) else {
        return false;
    };
    let mut sources = Vec::new();
    if let Ok(coin) = read_wav(coin) {
        sources.push(coin);
    }
    sources.push((speech, rate as u32));
    play_sources(sources).is_ok()
}

/// Ucapkan `text` dengan mesin TTS OS (crate `tts`); voice dipilih mengikuti `language`.
pub fn speak_os(language: &str, text: &str) -> bool {
    let Ok(mut tts) = tts::Tts::default() else { return false };
    if let Ok(voices) = tts.voices() {
        if let Some(voice) = pick_voice(&voices, language) {
            let _ = tts.set_voice(voice);
        }
    }
    tts.speak(text, true).is_ok()
}

/// Pilih voice sesuai bahasa; bila tidak ada yang cocok, biarkan voice bawaan OS.
fn pick_voice<'a>(voices: &'a [tts::Voice], language: &str) -> Option<&'a tts::Voice> {
    voices
        .iter()
        .find(|v| v.language().as_str().to_lowercase().starts_with(language))
        .or_else(|| {
            let hint = if language == "id" { "indonesia" } else { "english" };
            voices.iter().find(|v| v.name().to_lowercase().contains(hint))
        })
}

/// Putar denting koin saja (tanpa ucapan); untuk jalur fallback TTS OS.
pub fn play_coin(coin: &[u8]) -> Result<(), String> {
    let (samples, rate) = read_wav(coin)?;
    play_sources(vec![(samples, rate)])
}

/// Putar daftar (sampel, sample rate) secara berurutan sampai selesai.
/// Mixer rodio otomatis menyeragamkan kanal dan sample rate ke perangkat keluaran.
/// Aliran yang rusak (mis. ALSA EIO saat perangkat baru saja sibuk) dicoba ulang sekali.
fn play_sources(sources: Vec<(Vec<f32>, u32)>) -> Result<(), String> {
    let mut error = play_once(&sources);
    if error.is_err() {
        eprintln!("Layar Pelanggan: aliran audio bermasalah, mencoba ulang");
        error = play_once(&sources);
    }
    error
}

fn play_once(sources: &[(Vec<f32>, u32)]) -> Result<(), String> {
    let failed = Arc::new(AtomicBool::new(false));
    let flag = failed.clone();
    let mut device = rodio::DeviceSinkBuilder::from_default_device()
        .map_err(|e| e.to_string())?
        .with_error_callback(move |err| {
            eprintln!("Layar Pelanggan: galat aliran audio ({err})");
            flag.store(true, Ordering::SeqCst);
        })
        .open_stream()
        .map_err(|e| e.to_string())?;
    device.log_on_drop(false);
    let player = rodio::Player::connect_new(device.mixer());
    for (samples, rate) in sources {
        let rate = NonZeroU32::new(*rate).expect("sample rate tidak boleh nol");
        player.append(rodio::buffer::SamplesBuffer::new(NonZeroU16::new(1).unwrap(), rate, samples.clone()));
    }
    player.sleep_until_end();
    if failed.load(Ordering::SeqCst) {
        Err("aliran audio melaporkan galat".into())
    } else {
        Ok(())
    }
}

/// Bacakan WAV 16-bit/float (mono atau stereo) jadi sampel f32 + sample rate.
fn read_wav(bytes: &[u8]) -> Result<(Vec<f32>, u32), String> {
    let reader = hound::WavReader::new(Cursor::new(bytes)).map_err(|e| e.to_string())?;
    let rate = reader.spec().sample_rate;
    let samples: Vec<f32> = match reader.spec().sample_format {
        hound::SampleFormat::Float => {
            reader.into_samples::<f32>().collect::<Result<_, _>>().map_err(|e| e.to_string())?
        }
        hound::SampleFormat::Int => reader
            .into_samples::<i16>()
            .map(|v| v.map(|s| s as f32 / 32768.0))
            .collect::<Result<_, _>>()
            .map_err(|e| e.to_string())?,
    };
    Ok((samples, rate))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coin_wav_terbaca() {
        let coin = include_bytes!("../assets/coin.wav");
        let (samples, rate) = read_wav(coin).expect("coin.wav harus valid");
        assert_eq!(rate, 44100);
        assert!(samples.len() > 30000);
        // normalisasi tidak menyentuh puncak
        assert!(samples.iter().all(|s| s.abs() <= 1.0));
    }
}
