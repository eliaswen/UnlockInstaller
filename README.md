# UnlockInstaller

A desktop app for Windows and Linux that installs the SmokeAPI or CreamAPI DLC unlockers into Steam games.
It is written in Rust with GTK4 ([gtk4-rs](https://gtk-rs.org)).

- On **Windows** it handles the games in your Steam libraries.
- On **Linux** it handles both Windows games run through Proton and native Linux games.

**No unlocker binaries are included.** Open **Settings** (main menu, top right) and for each unlocker either:

- click **Download** to fetch the latest official release from GitHub
  ([acidicoala/SmokeAPI](https://github.com/acidicoala/SmokeAPI), [acidicoala/Koaloader](https://github.com/acidicoala/Koaloader)), or
- click **Import…** and pick a release archive you downloaded yourself. CreamAPI has no official GitHub releases, so it is import-only.

Supported archive formats are zip, 7z, rar and tar (plain, `.gz`, `.bz2`, `.xz`, `.zst`). The format is detected from the file's contents, not its extension.
zip, 7z and rar archives may be password-protected. A few common passwords are tried first, and if they fail you are asked.

## Build & run

```sh
cargo run --release
```

GTK4 must be installed first:

- **Linux**: the GTK4 development package (`sudo dnf install gtk4-devel` on Fedora, `libgtk-4-dev` on Debian/Ubuntu).
- **Windows**: follow the [gtk-rs Windows instructions](https://gtk-rs.org/gtk4-rs/stable/latest/book/installation_windows.html).
  The simplest route is MSYS2 with the `x86_64-pc-windows-gnu` Rust toolchain:
  `pacman -S mingw-w64-x86_64-gtk4 mingw-w64-x86_64-gcc mingw-w64-x86_64-pkgconf`.
  To run the built `.exe` outside the MSYS2 shell, the GTK DLLs (`C:\msys64\mingw64\bin`) must be on `PATH` or copied next to it.

### Cross-compiling for Windows from Linux

On Fedora:

```sh
sudo dnf install mingw64-gcc mingw64-gtk4
rustup target add x86_64-pc-windows-gnu
PKG_CONFIG=x86_64-w64-mingw32-pkg-config PKG_CONFIG_ALLOW_CROSS=1 \
  cargo build --release --no-default-features --target x86_64-pc-windows-gnu
```

The result is `target/x86_64-pc-windows-gnu/release/unlockinstaller.exe`. To run it on Windows it needs the GTK DLLs
from `/usr/x86_64-w64-mingw32/sys-root/mingw/bin` next to it.

`--no-default-features` turns off the `rar` feature: the build script of the RAR library decides what to compile from the
*host* OS, so it cannot be cross-compiled. A cross-compiled build therefore reads zip, 7z and tar archives but not rar.
Builds made on Windows itself keep rar support.

The Windows tests can be run under Wine (use a throwaway prefix; the settings test writes to the registry):

```sh
WINEPREFIX=/tmp/ui-wine WINEPATH=/usr/x86_64-w64-mingw32/sys-root/mingw/bin EXPECT_WINE=1 \
  PKG_CONFIG=x86_64-w64-mingw32-pkg-config PKG_CONFIG_ALLOW_CROSS=1 \
  CARGO_TARGET_X86_64_PC_WINDOWS_GNU_RUNNER=wine \
  cargo test --no-default-features --target x86_64-pc-windows-gnu
```

Wine is only for running those tests. The app itself should not be used under Wine or Proton: the Windows build shows a
warning when it detects them, because the native Linux build is the one that handles Proton games.

## Features

- **Scan**: reads `libraryfolders.vdf` and `appmanifest_*.acf` from every Steam library.
  - Windows: Steam is located through the registry, falling back to `Program Files (x86)\Steam`.
  - Linux: native, Flatpak and Snap Steam installs.
  - It lists games that ship `steam_api(64).dll` or `libsteam_api.so`.
  - Steam tools such as Proton and the Steam Linux Runtime are skipped.
- **Detection** of existing installs, by inspecting the files themselves:
  - direct installs (`*_o.dll` / `libsteam_api_o.so` backups), identified as SmokeAPI or CreamAPI from the DLL contents;
  - Koaloader proxy installs (a Koaloader proxy DLL next to the executable, plus SmokeAPI).
- **DLC lists** from `api.steamcmd.net` and the Steam Store.
  - Cached for 7 days; **Refresh** bypasses the cache.
  - If Steam can't be reached, the stale cache is used.
  - Your DLC selections are saved per game.
- **Install methods**
  - **Direct** (SmokeAPI "proxy mode" / classic CreamAPI): renames `steam_api(64).dll` to `steam_api(64)_o.dll`
    (or `libsteam_api.so` to `libsteam_api_o.so` for native Linux games) and puts the unlocker in its place.
    - CreamAPI gets a `cream_api.ini` listing the enabled DLCs.
    - SmokeAPI gets a `SmokeAPI.config.json`, but only when some DLCs are disabled (`override_dlc_status: locked`).
  - **Koaloader** (SmokeAPI only, Windows games): puts a Koaloader proxy DLL (e.g. `version.dll`) plus
    `SmokeAPI32/64.dll` next to each game executable.
    - On Windows nothing else is needed.
    - Under Proton, Wine prefers its builtin version of system DLLs, so the game's Steam launch options must contain
      `WINEDLLOVERRIDES="version=n,b" %command%`. The app shows this line with a Copy button.
- **Uninstall**: restores the original libraries and removes the unlocker, Koaloader and config/log files.
  It works per game, or across every game with **Uninstall from all games** in the main menu.
- Installing over an existing unlocker uninstalls it first, so you can switch between unlockers and methods.
- **Settings**: manage the imported unlockers, choose the default unlocker and install method, and change the
  folders described below.

Close a game before installing or uninstalling: on Windows its DLLs cannot be replaced while it is running.

## Settings and folders

| | Linux default | Windows default |
|---|---|---|
| Settings | `~/.config/UnlockInstaller/settings.json` | registry, `HKEY_CURRENT_USER\Software\UnlockInstaller` |
| Saved DLC selections (`selections.json`) | `~/.config/UnlockInstaller/` | `%APPDATA%\UnlockInstaller\` |
| Unlocker files (extracted archives) | `~/.local/share/UnlockInstaller/` | `%APPDATA%\UnlockInstaller\` |
| Cache (DLC lists, downloads) | `~/.cache/UnlockInstaller/` | `%LOCALAPPDATA%\UnlockInstaller\` |

- The **unlocker files** and **cache** folders can be changed under **Settings → Folders** (**Reset** goes back to the default).
  The choice is saved with the other settings: in `settings.json` on Linux, and as the `ExtractPath` / `CachePath`
  registry values on Windows. Files already in the old folder are not moved, so re-import the unlockers or move them yourself.
- On Linux, the **config folder** can be changed by starting the app with the `UNLOCKINSTALLER_CONFIG_DIR` environment
  variable set, e.g. `UNLOCKINSTALLER_CONFIG_DIR=~/games/unlock-config unlockinstaller`.

## Tests

```sh
cargo test                            # unit tests
# Live tests: scan your Steam install, query Steam and GitHub, download the real
# SmokeAPI/Koaloader releases and install them into a fake game folder. On Linux,
# point the data/cache dirs somewhere disposable so your own imported files are untouched:
XDG_DATA_HOME=/tmp/ui-data XDG_CACHE_HOME=/tmp/ui-cache cargo test -- --ignored --nocapture --test-threads=1
```

## Not yet implemented

- Batch install across several games
- Native-Linux SmokeAPI "hook mode" (`LD_PRELOAD`); native Linux games use direct mode
