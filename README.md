<p align="center">
  <a href="https://github.com/Raflyf/transparentbar">
    <img src="https://raw.githubusercontent.com/Raflyf/transparentbar/main/assets/transparentbar.png" width="88" height="88" alt="TransparentBar logo">
  </a>
</p>

<h1 align="center">TransparentBar</h1>

<p align="center">Native taskbar transparency for Windows. Built with Rust.</p>

<p align="center">
  <a href="https://github.com/Raflyf/transparentbar/actions/workflows/build.yml"><img src="https://github.com/Raflyf/transparentbar/actions/workflows/build.yml/badge.svg" alt="Windows build status"></a>
  <a href="https://github.com/Raflyf/transparentbar/releases/latest"><img src="https://img.shields.io/github/v/release/Raflyf/transparentbar?color=285ac7" alt="Latest release"></a>
  <a href="https://github.com/Raflyf/transparentbar/blob/main/LICENSE.md"><img src="https://img.shields.io/github/license/Raflyf/transparentbar" alt="MIT license"></a>
</p>

<p align="center">
  <a href="#download">Download</a> ·
  <a href="#preview">Preview</a> ·
  <a href="#resource-use">Resource use</a> ·
  <a href="#getting-started">Getting started</a> ·
  <a href="#compatibility">Compatibility</a> ·
  <a href="#building-from-source">Build</a> ·
  <a href="#contributing">Contribute</a>
</p>

TransparentBar removes the taskbar background while keeping your Windows icons and controls in place. It runs in the system tray, with optional startup at sign-in and a native menu for the settings you need.

## Preview

Your wallpaper stays visible behind pinned apps and system controls. This framed preview uses a real Windows 11 desktop screenshot, with the taskbar detail enlarged from the same capture.

![TransparentBar on a Windows 11 desktop, shown in a presentation frame with an enlarged taskbar detail](<https://raw.githubusercontent.com/Raflyf/TransparentBar/main/assets/desktop-preview.png>)

## Features

- **Transparent taskbar.** A clear background without blur or color effects.
- **Maximized-window connector.** An optional soft gradient strip follows the visible bottom edge of a maximized window, preserving different colors across its width and updating as content scrolls. Toggle it on or off from the tray menu. Fullscreen windows hide the strip.
- **Native Rust.** No webview, browser engine, or bundled UI framework.
- **Conditional refresh.** Taskbar transparency responds to Windows events. The connector samples a four-pixel-high edge every 120 ms only while a supported maximized window is active, and uploads its gradient only when colors or geometry change. While inactive, an enabled connector checks window eligibility once per second without capturing pixels; disabling it stops that timer.
- **Optional startup.** Enable or disable launch at sign-in from the tray menu. Off by default.
- **Release update check.** Checks GitHub Releases for a newer version and notifies you from the system tray, with a manual check option in the tray menu.
- **Installer or portable.** Install for your account, or extract a ZIP and run it.
- **No accounts or telemetry.** Runs locally with no user accounts or analytics; network access is used only to check GitHub Releases for updates.

## Resource use

A user-shared Task Manager capture shows **1.6 MB of memory** for the TransparentBar host process. The annotated view below highlights the application row while keeping the surrounding context visible.

| Memory | CPU | Disk | Network |
| --- | --- | --- | --- |
| **1.6 MB** | **0%** | **0 MB/s** | **0 Mbps** |

![Annotated Task Manager capture highlighting TransparentBar: 1.6 MB memory, 0 percent CPU, 0 MB per second disk, and 0 Mbps network at capture time](<https://raw.githubusercontent.com/Raflyf/TransparentBar/main/assets/resource-usage-card.png>)

These are displayed values at capture time, not a benchmark or an upper limit. CPU is rounded by Task Manager. Usage varies with Windows version and activity; the highlighted row covers the host process, not the bridge loaded inside Explorer. The frame, callout, and labels are presentation additions; the screenshot values are unchanged.

## Download

| Package | Best for |
| --- | --- |
| [Windows installer](https://github.com/Raflyf/transparentbar/releases/latest/download/TransparentBar-setup.exe) | Start Menu integration and a standard uninstall entry |
| [Portable ZIP](https://github.com/Raflyf/transparentbar/releases/latest/download/TransparentBar-windows-x64.zip) | Running without installation |

**Requires Windows 10 or Windows 11 on an x64 PC.** No administrator access is required for normal use. ARM64 is not supported.

Browse [all releases](https://github.com/Raflyf/transparentbar/releases) for earlier versions and release notes. Development builds are available in [GitHub Actions](https://github.com/Raflyf/transparentbar/actions).

## Getting started

1. Download the installer or extract the entire portable ZIP to a permanent folder.
2. Close any other taskbar customization tools, then open TransparentBar.
3. Click its icon in the system tray near the clock. If hidden, open the tray overflow menu.

| Tray option | Action |
| --- | --- |
| **Taskbar connector** | Toggle the maximized-window gradient connector strip |
| **Run at sign-in** | Toggle startup at Windows sign-in |
| **Check for updates** / **Update available** | Check GitHub Releases for a newer version, or open the latest release page when an update is found |
| **About / license** | View version and license information |
| **Exit** | Restore the saved appearance and exit |

The tray menu uses English labels, supports keyboard navigation, and adapts to the Windows system dark or light theme. Opening the application again opens the existing instance's menu.

For portable use, keep the executable and its versioned bridge DLL together. If you move the folder after enabling startup, disable and re-enable startup from the new location.

The connector currently supports the primary bottom-docked taskbar with auto-hide off. It samples a thin visible screen band above its own overlay, averages four rows, and smooths nearby columns to reduce text noise. It is a softened approximation of that edge, not a capture of the entire application. Pixels stay in memory and are never saved or uploaded. Unsupported taskbar placements and fullscreen windows hide the connector.

## Compatibility

TransparentBar detects the taskbar implementation rather than relying on a fixed Windows build number. It includes separate paths for legacy and XAML taskbars, with UI-thread handling for XAML windows.

This is an **early release**. Windows updates can change taskbar behavior; compatibility with every Windows build or third-party shell is not guaranteed. Multi-monitor behavior needs validation on the Windows version you use. If a taskbar update breaks transparency, please [report the issue](https://github.com/Raflyf/transparentbar/issues/new).

<details>
<summary><strong>Windows security warnings</strong></summary>

Current release binaries are unsigned. Windows may show an unknown-publisher or SmartScreen warning. Download only from this repository's Releases page, and follow your organization's security policy. Do not disable antivirus or Windows protection to run the application.

The build pipeline supports Authenticode signing when the maintainer provides a trusted code-signing certificate. A self-signed certificate does not establish public trust.

</details>

<details>
<summary><strong>Taskbar restoration and updates</strong></summary>

Choose **Exit** to restore the saved taskbar appearance before closing. The XAML bridge also watches for the host process to exit. If Windows rejects restoration, the application reports an error rather than discarding the saved state.

Windows keeps the XAML bridge loaded in Explorer for the remainder of that Explorer session. A versioned DLL allows application updates without overwriting the loaded image. TransparentBar does not replace Windows system files.

</details>

## Troubleshooting

If transparency does not activate, include the following in an [issue](https://github.com/Raflyf/transparentbar/issues/new):

- TransparentBar version and whether you used the installer or portable package.
- Windows version and build number from `winver`.
- The complete error message or a screenshot.
- Monitor count and any taskbar customization tools in use.

Startup diagnostics are saved to `%LOCALAPPDATA%\TransparentBar\startup.log`. Review the file before sharing it. For a diagnostic run that inspects XAML elements without changing their opacity, run from the application folder:

```powershell
.\transparentbar.exe --diagnose
```

## Building from source

Use Rust stable with the **x86_64-pc-windows-msvc** toolchain, Visual Studio C++ build tools, and the Windows SDK. The SDK resource compiler embeds the application icon and version information.

```powershell
git clone https://github.com/Raflyf/transparentbar.git
cd transparentbar
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
cargo build --release --locked
.\build.ps1 -SkipBuild
```

The portable package is created in `dist\TransparentBar-windows-x64.zip`. To build the installer, install Inno Setup and compile [setup.iss](https://github.com/Raflyf/transparentbar/blob/main/setup.iss). GitHub Actions builds both packages; version tags publish them to Releases.

GNU toolchain builds are available for development, but the release pipeline uses MSVC to embed Windows executable resources.

## Contributing

Bug reports, compatibility results, and focused pull requests are welcome. Check [existing issues](https://github.com/Raflyf/transparentbar/issues) before opening a new one, keep changes scoped, and run formatting, Clippy, and tests before submitting a pull request.

The project focuses on taskbar transparency and low background overhead. Discuss new features in an issue before adding UI frameworks or runtime dependencies.

### Release policy

New features and bug fixes receive a new release version. Bug-fix releases increment the patch version. Design-only updates, documentation, and other maintenance keep the current version; the commit SHA and CI run identify each build. Portable packages include `BUILD.txt` with the version and source commit. Each release displays its changelog directly; the source notes are maintained in [release-notes](https://github.com/Raflyf/TransparentBar/tree/main/release-notes).

## License

[MIT](https://github.com/Raflyf/transparentbar/blob/main/LICENSE.md) © Rafly Firmansyah.
