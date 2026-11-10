#!/usr/bin/env bash
# Build an AppImage from an existing release build.
#
#   cargo build --release && packaging/build-appimage.sh <output.AppImage>
#
# Needs: wget, file, patchelf, librsvg2-dev and the GTK4 development files.
# linuxdeploy and its GTK plugin are downloaded into target/appimage/.
set -euo pipefail

out=$(realpath -m "$1")
root=$(cd "$(dirname "$0")/.." && pwd)
work=$root/target/appimage
rm -rf "$work/AppDir"
mkdir -p "$work"
cd "$work"

[[ -x linuxdeploy-x86_64.AppImage ]] || {
    wget -q https://github.com/linuxdeploy/linuxdeploy/releases/download/continuous/linuxdeploy-x86_64.AppImage
    chmod +x linuxdeploy-x86_64.AppImage
}
[[ -x linuxdeploy-plugin-gtk.sh ]] || {
    wget -q https://raw.githubusercontent.com/linuxdeploy/linuxdeploy-plugin-gtk/master/linuxdeploy-plugin-gtk.sh
    chmod +x linuxdeploy-plugin-gtk.sh
}

# No FUSE in CI containers: let the AppImage tools unpack themselves instead.
export APPIMAGE_EXTRACT_AND_RUN=1
export DEPLOY_GTK_VERSION=4
export LDAI_OUTPUT=$out OUTPUT=$out

./linuxdeploy-x86_64.AppImage \
    --appdir AppDir \
    --executable "$root/target/release/unlockinstaller" \
    --desktop-file "$root/packaging/io.github.UnlockInstaller.desktop" \
    --icon-file "$root/packaging/io.github.UnlockInstaller.svg" \
    --plugin gtk \
    --output appimage
echo "Wrote $out"
