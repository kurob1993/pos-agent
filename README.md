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
{"ok":true,"version":"0.1.0","monitor":true}

{"type":"open","url":"https://pos.example.com/customer-display"}
{"ok":true}

{"type":"close"}
{"ok":true}
```

`url` harus berasal dari origin yang sama dengan halaman pengirim. / `url` must have the same origin as the sending page.

`close` menutup layar pelanggan dan melupakan alamat terakhir (tidak tampil lagi saat login berikutnya). Hanya situs yang sudah diizinkan yang boleh memakainya.
`close` closes the customer display and forgets the last address (it won't reappear at next login). Only already-allowed sites may use it.

Konfigurasi disimpan di `config.json` pada direktori konfigurasi aplikasi. / Settings are stored in `config.json` in the app config directory.

> Host selain `127.0.0.1`/`localhost` membuat WebSocket bisa dijangkau dari jaringan; gunakan hanya bila perlu.
> A host other than `127.0.0.1`/`localhost` exposes the WebSocket to the network; use it only if needed.

## Pengembangan / Development

```sh
npm install
npm run dev     # tauri dev
npm run build   # tauri build
```
