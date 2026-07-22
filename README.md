# REDR (Remove Empty Directories)
A lightning-fast GUI and CLI tool written in Rust for finding and safely cleaning up empty directories.
Built with [Slint](https://slint.dev/) for a native UI and [Rayon](https://github.com/rayon-rs/rayon) for multi-threading.

## ✨ Features
- **Extreme Speed:** Parallel scanning with an experimental **Direct MFT Scan** for Windows (reads NTFS tables directly from disk for instant results).
- **Smart Safety:** Moves folders to Trash/Recycle Bin by default. Automatically protects system folders, hidden files, and visualizes Symlinks/Junctions.
- **Advanced Filtering:** Use Glob patterns (e.g., `*.tmp`), set minimum folder age, and ignore 0-byte files.
- **Interactive GUI:** Modern dark-theme tree view with Drag & Drop and right-click context menus.
- **Automation Ready:** CLI support with `--dry-run`, `--quiet`, and `--json` reporting.
- **OS Integration:** Easily add a "Remove empty folders here" shortcut to the Windows Explorer context menu.

## 🛠️ Build
```
cargo build --release
```
