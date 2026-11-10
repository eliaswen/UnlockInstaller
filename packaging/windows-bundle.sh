#!/usr/bin/env bash
# Bundle a cross-compiled Windows build with the Qt runtime it needs.
#
#   packaging/windows-bundle.sh <unlockinstaller.exe> <output.zip>
#
# Lays files out the way windeployqt does: the exe, Qt's plugins (platforms/,
# imageformats/, ...) and QML modules (qml/) next to it, plus every MinGW DLL
# any of them (transitively) depends on. Needs the mingw64-qt6-* packages.
set -euo pipefail

exe=$1
out=$(realpath -m "$2")
sysroot=${MINGW_SYSROOT:-/usr/x86_64-w64-mingw32/sys-root/mingw}
objdump=${OBJDUMP:-x86_64-w64-mingw32-objdump}
qt_plugins=$sysroot/lib/qt6/plugins
qt_qml=$sysroot/lib/qt6/qml

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
stage=$work/UnlockInstaller
mkdir -p "$stage"
cp "$exe" "$stage/"

# Qt plugins: the Windows platform integration, image formats (game icons are
# .ico files) and the style and icon helpers the platform plugin loads.
for dir in platforms imageformats styles iconengines; do
    if [[ -d $qt_plugins/$dir ]]; then
        cp -r "$qt_plugins/$dir" "$stage/"
    fi
done

# QML modules used by qml/*.qml, with the modules they import in turn.
mkdir -p "$stage/qml"
for module in QtQml QtQuick; do
    cp -r "$qt_qml/$module" "$stage/qml/"
done
# Trim what the app never loads: other Controls styles, tests, 3D, etc.
rm -rf "$stage/qml/QtQuick/"{Controls/{Imagine,Material,Universal,FluentWinUI3,iOS,macOS},Scene2D,Scene3D,Particles,VirtualKeyboard,LocalStorage,Shapes/DesignHelpers,test,Timeline,Pdf,Studio} 2>/dev/null || true
rm -rf "$stage/qml/QtQml/"{RemoteObjects,StateMachine,XmlListModel} 2>/dev/null || true
find "$stage/qml" \( -name '*.qmltypes' -o -name '*.a' -o -name '*.prl' \) -delete

# The MinGW Qt was built with its Linux install paths; point it at the files
# next to the exe instead (plugins in platforms/ etc., QML modules in qml/).
printf '[Paths]\nPrefix = .\nPlugins = .\nQmlImports = qml\n' > "$stage/qt.conf"

# Walk the import tables of the exe and every plugin. DLLs that are not in the
# MinGW sysroot are part of Windows itself and are skipped.
mapfile -t queue < <(find "$stage" -iname '*.exe' -o -iname '*.dll')
declare -A seen=()
while ((${#queue[@]})); do
    file=${queue[0]}
    queue=("${queue[@]:1}")
    while read -r dll; do
        key=${dll,,}
        [[ -n ${seen[$key]:-} ]] && continue
        seen[$key]=1
        src=$(find "$sysroot/bin" -maxdepth 1 -iname "$dll" -print -quit)
        [[ -z $src ]] && continue
        cp "$src" "$stage/"
        queue+=("$src")
    done < <("$objdump" -p "$file" | awk '/DLL Name:/ { print $3 }')
done

rm -f "$out"
(cd "$work" && zip -qr "$out" UnlockInstaller)
echo "Wrote $out ($(du -h "$out" | cut -f1))"
