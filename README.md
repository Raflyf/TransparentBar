# TransparentBar

Taskbar transparan untuk Windows 10 dan Windows 11 x64. EXE dan bridge DLL dibuat native Rust. Tanpa browser runtime, blur, logger, telemetry, atau polling terus-menerus.

## Download

Ambil **TransparentBar-setup.exe** untuk instalasi, atau **TransparentBar-windows-x64.zip** untuk portable, dari [Releases](https://github.com/Raflyf/transparentbar/releases/latest). Build tiap push tersedia di [Actions](https://github.com/Raflyf/transparentbar/actions).

### Setup

1. Jalankan installer; instalasi berlaku untuk akun Windows aktif tanpa administrator.
2. Opsi **Jalankan saat login** tersedia di installer; default mati.
3. Buka TransparentBar dari Start Menu.

### Portable

1. Ekstrak seluruh ZIP ke folder tetap.
2. Jalankan `transparentbar.exe`. `transparentbar_bridge.dll` harus di folder yang sama.

### Pengaturan

Klik ikon TransparentBar di tray dekat jam; buka menu ikon tambahan bila tersembunyi. Menu native mendukung keyboard. Centang **Jalankan saat login** untuk startup; hilangkan centang untuk mematikannya. **Keluar** memulihkan taskbar lalu menghentikan aplikasi. Membuka EXE lagi membuka menu instance yang ada.

Startup hanya memakai nilai **TransparentBar** pada `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`. Folder ber-spasi/Unicode didukung; batas command Windows 260 karakter. Bila lokasi portable pindah, matikan lalu aktifkan startup dari lokasi baru. Task Manager dapat menonaktifkan startup secara terpisah.

## Kompatibilitas dan pemulihan

Deteksi struktur taskbar, bukan daftar build: legacy memakai composition attribute; XAML memakai Windows XAML Diagnostics dengan bridge Rust. Tidak ada pembatasan ke satu build Windows 11. Primary taskbar XAML memakai dispatcher pemilik, menyimpan opacity asli, mengembalikan saat keluar/proses host mati. Taskbar legacy di monitor tambahan ikut diproses. Explorer restart dikenali dan sambungan dibangun ulang.

API shell bukan kontrak stabil untuk semua update Windows. Build masa depan, termasuk 26H2, tidak dapat dijamin sebelum diuji. Shell pihak ketiga, ARM64, serta XAML multi-monitor dengan dispatcher terpisah perlu pengujian/adapter tersendiri. Unsupported taskbar memberi error, bukan klaim sukses palsu. Jangan menjalankan bersamaan dengan pengubah taskbar lain.

Bridge dimuat dalam Explorer sampai Explorer berakhir. Tidak mengganti file Windows. Pemulihan XAML menjaga nilai opacity asli; binding/style lain di luar cakupan. Jika Windows menolak akses atau taskbar berubah, aplikasi memberi error; jangan matikan antivirus atau menjalankan sebagai administrator sebagai solusi default.

Idle memakai blocking Windows message wait. Timer dipakai sementara untuk kesiapan awal dan event refresh, bukan loop polling permanen. RAM/CPU belum dibenchmark; tidak ada angka perkiraan sebagai klaim. Pengguna menguji instalasi, transparansi, login, dan monitor nyata.

## Build

Rust stable Windows x64; bindings Windows dipin versi kompatibel GNU/MSVC yang tersedia.

```powershell
cargo fmt --check
cargo test --locked
cargo build --release --locked
.\build.ps1 -SkipBuild
```

GitHub Actions menjalankan format, Clippy, tes, build MSVC dengan CRT statis, package ZIP, dan compiler Inno Setup. Tag `v*` menerbitkan ZIP dan setup di Releases. Compiler installer hanya dependency build; tidak berjalan saat aplikasi dipakai.

## Digital signing

Pipeline Authenticode tersedia melalui repo secrets **WINDOWS_CERTIFICATE_BASE64** (PFX code-signing resmi) dan **WINDOWS_CERTIFICATE_PASSWORD**. Sertifikat ditimestamp SHA-256 dan diverifikasi dengan `signtool verify /pa`. Jangan commit PFX/private key ke repo. Tanpa sertifikat resmi, build tetap unsigned dan Windows dapat memperingatkan penerbit tidak dikenal. Self-signed bukan pengganti trust publik; tanda tangan juga tidak menjamin bebas peringatan SmartScreen.

## Lisensi

[MIT License](<LICENSE.md>). Source host dan bridge tersedia di repo; tidak memuat komponen biner aplikasi referensi. API definitions mengikuti [Windows SDK XAML Diagnostics](https://learn.microsoft.com/en-us/windows/win32/api/xamlom/) dan [IUIElement](https://learn.microsoft.com/en-us/uwp/api/windows.ui.xaml.uielement.opacity).
