# Layar Pelanggan / Customer Display

Aplikasi tray untuk PC kasir yang menampilkan layar pelanggan POS layar penuh di monitor kedua.
A tray app for the cashier PC that shows the POS customer display fullscreen on a second monitor.

## Cara penggunaan (ID)

1. Hubungkan monitor kedua, atur layar ke mode **Extend**.
2. Jalankan aplikasi. Ikonnya ada di system tray (klik kanan: Pengaturan, Tampilkan ulang, Keluar).
3. Halaman kasir (POS) terhubung ke `ws://127.0.0.1:8183` dan mengirim alamat layar pelanggan.
4. Pertama kali, izinkan situs POS pada dialog yang muncul. Izin tersimpan.
5. Alamat terakhir diingat, jadi layar langsung tampil saat PC menyala.

Menu **Pengaturan** (tray): pilihan monitor, layar penuh/jendela beserta ukurannya, host dan port WebSocket, bahasa (Indonesia/English), dan mulai otomatis saat login.

## How to use (EN)

1. Connect the second monitor and set the displays to **Extend** mode.
2. Start the app. Its icon is in the system tray (right-click: Settings, Show again, Quit).
3. The cashier (POS) page connects to `ws://127.0.0.1:8183` and sends the customer display address.
4. The first time, allow the POS site in the dialog. The choice is saved.
5. The last address is remembered, so the display shows right after the PC starts.

**Settings** (tray menu): monitor choice, fullscreen/window mode and size, WebSocket host and port, language (Indonesian/English), and start-on-login.

## Protokol / Protocol

Pesan JSON lewat WebSocket / JSON messages over the WebSocket:

```json
{"type":"ping"}
{"ok":true,"version":"0.4.0","monitor":true}

{"type":"open","url":"https://pos.example.com/customer-display"}
{"ok":true}

{"type":"close"}
{"ok":true}

{"type":"speak","method":"cash","amount":50000}
{"ok":true}
```

`url` harus berasal dari origin yang sama dengan halaman pengirim. / `url` must have the same origin as the sending page.

### Pengumuman suara / Voice announcement

Perintah `speak` memutar denting koin lalu mengucapkan "Pembayaran berhasil …" melalui speaker PC kasir, misalnya untuk notifikasi pembayaran sukses tunai, EDC, dan QRIS. Nominal otomatis diterjemahkan menjadi kata (50.000 → "lima puluh ribu rupiah"). / The `speak` command plays a coin chime then announces "Payment successful …" through the cashier PC speakers, e.g. for successful cash, EDC, and QRIS payments. The amount is automatically converted to words.

- `method`: `cash`, `edc`, atau / or `qris`
- `amount`: angka positif / a positive number, maks. / max. 10¹⁵
- Suara hanya keluar dari situs yang sudah diizinkan (sama seperti `open`/`close`). / Speech only plays from allowed sites (same as `open`/`close`).
- Suara diputar thread terpisah; jawaban `ok` dikirim sebelum ucapan selesai. / Speech plays on a separate thread; the `ok` reply is sent before speaking finishes.

#### Cara integrasi di halaman kasir / How to integrate in the cashier page

```js
const ws = new WebSocket("ws://127.0.0.1:8183");
// panggil sekali di awal; izinkan saat dialog pertama muncul / call once at startup; allow when the first dialog appears

// setelah pembayaran berhasil / after a successful payment:
ws.send(JSON.stringify({ type: "speak", method: "qris", amount: 50000 }));
// → 🔔 denting koin, lalu / then: "Pembayaran berhasil, keris lima puluh ribu rupiah, terima kasih."
```

Kata QRIS sengaja ditulis "keris" agar mesin suara melafalkannya seperti senjata tradisional, bukan "kuris". / QRIS is deliberately written "keris" so the voice says it like the traditional weapon, not "kuris".

#### Cara memasang model suara natural (opsional, disarankan) / How to install the natural voice model (optional, recommended)

Aplikasi memakai suara natural **Supertonic 3 Bahasa Indonesia** bila modelnya tersedia; tanpa model, jatuh ke mesin TTS OS. / The app uses the natural **Supertonic 3 Indonesian** voice when the model is present; otherwise it falls back to the OS TTS engine.

**Cara termudah:** buka **Pengaturan** di aplikasi lalu klik tombol **Unduh suara natural (~139 MB)**. Progres tampil di situ; butuh internet sekali saja. / **Easiest way:** open **Settings** in the app and click the **Download natural voice (~139 MB)** button. Progress is shown there; needs the internet once.

Cara manual (tanpa tombol): / Manual steps (without the button):

```sh
# Linux (jalankan sebagai pengguna yang sama dengan yang menjalankan aplikasi / run as the same user that runs the app)
wget -O /tmp/tts.tar.bz2 https://github.com/k2-fsa/sherpa-onnx/releases/download/tts-models/sherpa-onnx-supertonic-3-tts-int8-2026-05-11.tar.bz2
mkdir -p ~/.config/id.co.krakatau-it.pos.layar-pelanggan
tar -xjf /tmp/tts.tar.bz2 -C ~/.config/id.co.krakatau-it.pos.layar-pelanggan
mv ~/.config/id.co.krakatau-it.pos.layar-pelanggan/sherpa-onnx-supertonic-3-tts-int8-2026-05-11 \
   ~/.config/id.co.krakatau-it.pos.layar-pelanggan/tts-model
```

Windows: ekstrak isi arsip ke `%APPDATA%\id.co.krakatau-it.pos.layar-pelanggan\tts-model\` (folder yang sama dengan `config.json`). / On Windows, extract the archive into `%APPDATA%\id.co.krakatau-it.pos.layar-pelanggan\tts-model\` (the folder that also holds `config.json`).

- Model ±139 MB, 100% offline, lisensi OpenRAIL-M (berkas LICENSE ikut dalam arsip). / The model is ±139 MB, fully offline, OpenRAIL-M licensed (LICENSE file included in the archive).
- Pengumuman pertama setelah aplikasi berjalan lebih lambat (memuat model ±beberapa detik); setelahnya langsung. / The first announcement after the app starts is slower (loading the model, ±seconds); subsequent ones are instant.
- Tanpa model: Windows perlu voice Bahasa Indonesia dipasang sekali (Settings → Time & Language → Speech, mis. "Microsoft Gadis"); Linux memakai speech-dispatcher/espeak yang terdengar robotik. / Without the model: Windows needs an Indonesian voice installed once (Settings → Time & Language → Speech, e.g. "Microsoft Gadis"); Linux uses speech-dispatcher/espeak which sounds robotic.
- Uji tanpa membuka POS: `npm run test:voice` (dari folder proyek, untuk pengembang). / Test without the POS: `npm run test:voice` (from the project folder, for developers).

`close` menutup layar pelanggan dan melupakan alamat terakhir (tidak tampil lagi saat login berikutnya). Hanya situs yang sudah diizinkan yang boleh memakainya.
`close` closes the customer display and forgets the last address (it won't reappear at next login). Only already-allowed sites may use it.

Konfigurasi disimpan di `config.json` pada direktori konfigurasi aplikasi. / Settings are stored in `config.json` in the app config directory.

> Host selain `127.0.0.1`/`localhost` membuat WebSocket bisa dijangkau dari jaringan; gunakan hanya bila perlu.
> A host other than `127.0.0.1`/`localhost` exposes the WebSocket to the network; use it only if needed.

## Pengembangan / Development

```sh
npm install
npm run dev         # tauri dev
npm run build       # tauri build
npm run test:voice  # uji suara pengumuman pembayaran (butuh libspeechd-dev di Linux)
```
