# UnlockInstaller

A desktop app for Windows and Linux that installs the SmokeAPI or CreamAPI DLC unlockers into Steam games.
It is written in Rust with a Qt 6 / QML interface, using [CXX-Qt](https://github.com/KDAB/cxx-qt).

- On **Windows** it handles the games in your Steam libraries.
- On **Linux** it handles both Windows games run through Proton and native Linux games.

**No unlocker binaries are included.** Open **Settings** (main menu, top right) and for each unlocker either:

- click **Download** to fetch the latest official release from GitHub
  ([acidicoala/SmokeAPI](https://github.com/acidicoala/SmokeAPI), [acidicoala/Koaloader](https://github.com/acidicoala/Koaloader)), or
- click **Import…** and pick a release archive you downloaded yourself. CreamAPI has no official GitHub releases, so it is import-only.

Supported archive formats are zip, 7z, rar and tar (plain, `.gz`, `.bz2`, `.xz`, `.zst`). The format is detected from the file's contents, not its extension.
zip, 7z and rar archives may be password-protected. A few common passwords are tried first, and if they fail you are asked.

## Install

Download the latest version from the [releases page](https://github.com/eliaswen/UnlockInstaller/releases/latest):

- **Windows**: `UnlockInstaller-x.y.z-windows-x86_64.zip`. Unzip it anywhere and run `unlockinstaller.exe`.
  This build cannot read rar archives.
- **Linux, any distribution**: `UnlockInstaller-x.y.z-x86_64.AppImage`. Make it executable and run it. Needs glibc 2.39 or newer.
- **Fedora**: the `.rpm`, or add the dnf repository to also get updates:

  ```sh
  sudo curl -fsSLo /etc/yum.repos.d/unlockinstaller.repo https://eliaswen.github.io/UnlockInstaller/rpm/unlockinstaller.repo
  sudo dnf install unlockinstaller
  ```

- **Debian 13+ / Ubuntu 24.04+**: the `.deb`, or add the apt repository to also get updates:

  ```sh
  sudo curl -fsSLo /usr/share/keyrings/unlockinstaller.gpg https://eliaswen.github.io/UnlockInstaller/key.gpg
  echo "deb [signed-by=/usr/share/keyrings/unlockinstaller.gpg] https://eliaswen.github.io/UnlockInstaller/apt ./" | sudo tee /etc/apt/sources.list.d/unlockinstaller.list
  sudo apt update
  sudo apt install unlockinstaller
  ```

The app checks GitHub for a newer release when it starts and shows a link to it; it never downloads or installs
anything by itself. The check can be turned off in **Settings**, and run by hand from the main menu.

## Build & run

```sh
cargo run --release
```

Qt 6 (6.4 or newer) with Qt Quick must be installed first. The build finds it through `qmake6` (or `qmake`) on `PATH`,
or the `QMAKE` environment variable.

- **Fedora**: `sudo dnf install gcc-c++ qt6-qtbase-devel qt6-qtdeclarative-devel`
- **Debian/Ubuntu**: `sudo apt install g++ qt6-base-dev qt6-declarative-dev qt6-declarative-dev-tools qml6-module-qtquick
  qml6-module-qtquick-controls qml6-module-qtquick-templates qml6-module-qtquick-layouts qml6-module-qtquick-dialogs
  qml6-module-qtquick-window qml6-module-qtqml-workerscript`
- **Windows**: install Qt 6 for MinGW with the [Qt online installer](https://www.qt.io/download-qt-installer), use the
  `x86_64-pc-windows-gnu` Rust toolchain and point `QMAKE` at that Qt's `bin\qmake6.exe`. To run the built `.exe`, run
  `windeployqt --qmldir qml unlockinstaller.exe` on it once.

The windows are described in `qml/` and compiled into the binary; `src/ui.rs` holds the state behind them.
The controls use Qt's Fusion style (set `QT_QUICK_CONTROLS_STYLE` to use another one).

### Cross-compiling for Windows from Linux

On Fedora:

```sh
sudo dnf install mingw64-gcc-c++ mingw64-qt6-qtbase mingw64-qt6-qtdeclarative
rustup target add x86_64-pc-windows-gnu
QMAKE=/usr/bin/x86_64-w64-mingw32-qmake-qt6 CXX_QT_AUTORCC_OPTIONS=--no-zstd \
  cargo build --release --no-default-features --target x86_64-pc-windows-gnu
packaging/windows-bundle.sh target/x86_64-pc-windows-gnu/release/unlockinstaller.exe UnlockInstaller-windows.zip
```

The result is `target/x86_64-pc-windows-gnu/release/unlockinstaller.exe`. `packaging/windows-bundle.sh` zips it with
the Qt DLLs, plugins and QML modules it needs.

`--no-default-features` turns off the `rar` feature: the build script of the RAR library decides what to compile from the
*host* OS, so it cannot be cross-compiled. A cross-compiled build therefore reads zip, 7z and tar archives but not rar.
Builds made on Windows itself keep rar support.

The Windows tests can be run under Wine (use a throwaway prefix; the settings test writes to the registry):

```sh
WINEPREFIX=/tmp/ui-wine EXPECT_WINE=1 WINEPATH=/usr/x86_64-w64-mingw32/sys-root/mingw/bin \
  QMAKE=/usr/bin/x86_64-w64-mingw32-qmake-qt6 CARGO_TARGET_X86_64_PC_WINDOWS_GNU_RUNNER=wine \
  cargo test --no-default-features --target x86_64-pc-windows-gnu
```

Wine is only for running those tests. The app itself should not be used under Wine or Proton: the Windows build shows a
warning when it detects them, because the native Linux build is the one that handles Proton games. To make sure that
warning is readable, under Wine the app draws in software (`QT_QUICK_BACKEND=software`) and uses the Noto Sans font
bundled in `assets/fonts/`.

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

## Releasing

Releases are made by [.github/workflows/ci.yml](.github/workflows/ci.yml). To publish version `x.y.z`:

1. Set `version = "x.y.z"` in `Cargo.toml` and run `cargo build` so `Cargo.lock` follows.
2. Commit with the message `Release x.y.z` (exactly that as the first line) and push to `main`.

The workflow then runs the tests, builds the Windows `.zip`, the `.rpm`, the `.deb` and the AppImage, and creates the
GitHub release `vx.y.z` with those four files and the list of commits since the previous release. Finally it rebuilds
the dnf and apt repositories from the packages of the five most recent releases and publishes them to GitHub Pages.
If the version in the commit message and in `Cargo.toml` differ, the run fails before building anything.

One-time setup for the dnf and apt repositories:

1. In the GitHub repository, **Settings → Pages → Build and deployment → Source**: choose **GitHub Actions**.
2. Create a signing key without a passphrase and store its private half as the `REPO_GPG_PRIVATE_KEY` repository secret
   (**Settings → Secrets and variables → Actions**):

   ```sh
   gpg --batch --passphrase '' --quick-generate-key "UnlockInstaller packages" ed25519 sign never
   gpg --armor --export-secret-keys "UnlockInstaller packages" | gh secret set REPO_GPG_PRIVATE_KEY
   ```

   Keep a backup of that key: users of the repositories trust it, so replacing it means they must re-add the repository.

Without those two steps the release itself still works; only the last job (the repositories) fails.

The pieces can be run by hand too: `packaging/windows-bundle.sh` (Windows zip), `packaging/build-appimage.sh`
(AppImage), `cargo deb`, `cargo generate-rpm` and `packaging/build-repos.sh` (repositories).

## Not yet implemented

- Batch install across several games
- Native-Linux SmokeAPI "hook mode" (`LD_PRELOAD`); native Linux games use direct mode
