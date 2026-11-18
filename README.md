# Veyra

A light, minimal web browser for Windows, written in Rust on Microsoft WebView2.

- **Vertical tabs** with pinned tiles, bookmarks, tab groups and a compact or auto-hide sidebar.
- **Built-in ad and tracker blocking** (Brave's adblock-rust with EasyList, EasyPrivacy and uBlock Origin filters).
- **Windows as you like them**: drag a tab out and it becomes a window with the page still alive; drop it on another window's sidebar to bring it back.
- **Customizable new tab page**: your own image or video, colors, gradients, position of the search box.
- **Low on resources**: background tabs are suspended and freed, one engine for all windows.
- **Italian and English**, light and dark theme, glass or solid look.
- No AI features, no accounts, no telemetry of its own.

## Download

Get `Veyra-x.y.z-setup.exe` from [Releases](https://github.com/MooseDevPJS/veyra/releases/latest). Windows 10/11 x64; it installs per user, no administrator rights needed. Veyra updates itself from **Settings → About → Check for updates**.

## Build

```
cargo build --release
"C:\Program Files (x86)\Inno Setup 6\ISCC.exe" installer\veyra.iss
```

Needs Rust (stable) and, for the installer, Inno Setup 6.

## License

MIT. Ad blocking uses adblock-rust (MPL-2.0); filter lists EasyList and EasyPrivacy (GPLv3 / CC BY-SA) and uBlock Origin resources (GPLv3).
