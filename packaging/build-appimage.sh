#!/usr/bin/env bash
# Build an AppImage from an existing release build.
#
#   cargo build --release && packaging/build-appimage.sh <output.AppImage>
#
# Needs: wget, file and patchelf, plus the Qt 6 the app was built against
# (found through qmake6, or QMAKE). linuxdeploy and its Qt plugin are
# downloaded into target/appimage/; the plugin copies Qt, its plugins and the
# QML modules that qml/*.qml imports into the AppImage.
set -euo pipefail

out=$(realpath -m "$1")
root=$(cd "$(dirname "$0")/.." && pwd)
work=$root/target/appimage
rm -rf "$work/AppDir"
mkdir -p "$work"
cd "$work"

for tool in linuxdeploy linuxdeploy-plugin-qt; do
    [[ -x $tool-x86_64.AppImage ]] || {
        wget -q "https://github.com/linuxdeploy/$tool/releases/download/continuous/$tool-x86_64.AppImage"
        chmod +x "$tool-x86_64.AppImage"
    }
done

# No FUSE in CI containers: let the AppImage tools unpack themselves instead.
export APPIMAGE_EXTRACT_AND_RUN=1
export LDAI_OUTPUT=$out OUTPUT=$out
export QMAKE=${QMAKE:-$(command -v qmake6 || command -v qmake)}
export QML_SOURCES_PATHS=$root/qml

./linuxdeploy-x86_64.AppImage \
    --appdir AppDir \
    --executable "$root/target/release/unlockinstaller" \
    --desktop-file "$root/packaging/io.github.UnlockInstaller.desktop" \
    --icon-file "$root/packaging/io.github.UnlockInstaller.svg" \
    --plugin qt \
    --output appimage
echo "Wrote $out"
