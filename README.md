# REDR (Remove Empty Directories)

Tool to find and safely remove empty directories.

## Features

- **Speed** — parallel walk, streaming mode (memory ∝ tree depth), incremental rescan (unchanged subtrees are reused, binary cache), optional Direct MFT Scan on Windows.
- **Safety** — recycle bin by default, dry-run on by default, double confirmation for mass deletion, whitelist (system folders by default), symlink/junction/reparse/OneDrive protection, deletion log with restore.
- **Filters** — glob patterns, file size and age, minimum folder age, depth, result limit, exclusions by name or path.
- **UI** — tree with search and grouping (tree / drive / top folder), dark and light themes, progress with ETA and speed, hotkeys (`F5` scan, `Ctrl+D` delete), context menu.
- **Export** — CSV and JSON results, log export. **Profiles** — named settings. **CLI** — `--dry-run`, `--quiet`, `--json`.

## Build

```
cargo build --release # target/release/REDR.exe
```

## Usage

```
REDR.exe --gui                              # GUI
REDR.exe --cli "C:\path"                    # scan only
REDR.exe --cli --delete --execute "C:\path" # delete (--execute required)
```

| Flag | Meaning |
|------|---------|
| `--execute` | Actually delete (without it: simulation) |
| `--trash-only` | Never delete permanently |
| `--protect-paths` | Comma-separated paths that must not be deleted |
| `--mft-scan` | Read the NTFS MFT directly (needs administrator) |
| `--json` / `--quiet` | JSON report / less output |

## Development

```
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

```
src/
  lib.rs, main.rs, cli.rs, config.rs, view.rs
  gui/        mod, state, helpers, actions, scan, delete, settings, export, view_controls
  scanner/    rules, walk, stream, incremental, mft, delete, filters, cache, binary_cache, types
translations/   gettext .po files
tests/          integration tests
```

## Localization

UI strings live in `translations/<lang>/LC_MESSAGES/REDR.po` and are compiled into the binary.
