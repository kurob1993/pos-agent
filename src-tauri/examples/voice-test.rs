//! Uji suara pengumuman pembayaran memakai mesin yang sama dengan aplikasi.
//! Jalankan dari root proyek: `npm run test:voice`
//! Denting koin + suara natural Supertonic bila model ada di folder konfigurasi,
//! selain itu jatuh ke mesin TTS OS.
use std::path::PathBuf;

// sebagian fungsi (mis. preload) hanya dipakai aplikasi
#[allow(dead_code)]
#[path = "../src/audio.rs"]
mod audio;
#[path = "../src/voice.rs"]
mod voice;

fn main() {
    // Lokasi model sama dengan yang dicari aplikasi: folder konfigurasi aplikasi (Linux)
    let config = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|_| std::env::var("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_default()
        .join("id.co.krakatau-it.pos.layar-pelanggan")
        .join("tts-model");
    let coin = include_bytes!("../assets/coin.wav");
    let model = audio::find_model_dir(&[config.clone()]);
    match &model {
        Some(dir) => println!(">> mesin: Supertonic ({})", dir.display()),
        None => println!(">> model tidak ada di {}, pakai TTS OS", config.display()),
    }
    // `npm run test:voice -- "teks"` mengucapkan teks itu saja (angka diubah jadi kata)
    if let Some(text) = std::env::args().nth(1) {
        let text = voice::normalize_text("id", &text);
        println!(">> \"{text}\"");
        match &model {
            Some(dir) => audio::speak_supertonic(dir, Some(coin), &text, 5, "id"),
            None => audio::speak_os("id", &text),
        };
        return;
    }
    let cases = [
        ("cash", 50000.0),
        ("edc", 125000.0),
        ("qris", 25000.5),
        ("cash", 1500.0),
        ("qris", 11000.0),
        ("edc", 1250750.0),
        ("cash", 99999.0),
        ("qris", 2000000.0),
    ];
    for (method, amount) in cases {
        let text = voice::payment_text("id", method, amount).unwrap();
        println!(">> {method} {amount}: \"{text}\"");
        let ok = match &model {
            Some(dir) => audio::speak_supertonic(dir, Some(coin), &text, 5, "id"),
            None => audio::play_coin(coin).is_ok() && audio::speak_os("id", &text),
        };
        if !ok {
            println!(">> gagal memutar suara");
        }
    }
    println!(">> semua ucapan selesai");
}
