#!/usr/bin/env bash
# Bundle a cross-compiled Windows build with the GTK runtime it needs.
#
#   packaging/windows-bundle.sh <unlockinstaller.exe> <output.zip>
#
# Copies the exe, every MinGW DLL it (transitively) depends on and the GTK
# data files into an `UnlockInstaller/` folder and zips it.
set -euo pipefail

exe=$1
out=$(realpath -m "$2")
sysroot=${MINGW_SYSROOT:-/usr/x86_64-w64-mingw32/sys-root/mingw}
objdump=${OBJDUMP:-x86_64-w64-mingw32-objdump}

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
stage=$work/UnlockInstaller
mkdir -p "$stage"
cp "$exe" "$stage/"

# gdk-pixbuf image loaders are plugins: ship them and their dependencies too.
loaders=()
if compgen -G "$sysroot/lib/gdk-pixbuf-2.0" >/dev/null; then
    mkdir -p "$stage/lib"
    cp -r "$sysroot/lib/gdk-pixbuf-2.0" "$stage/lib/"
    find "$stage/lib/gdk-pixbuf-2.0" -name '*.a' -delete
    mapfile -t loaders < <(find "$stage/lib/gdk-pixbuf-2.0" -name '*.dll')
fi

# Walk the import tables. DLLs that are not in the MinGW sysroot are part of
# Windows itself and are skipped.
declare -A seen=()
queue=("$exe" "${loaders[@]}")
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

# GSettings schemas (the file chooser aborts without them) and icons.
mkdir -p "$stage/share/glib-2.0/schemas"
cp "$sysroot"/share/glib-2.0/schemas/*.xml "$stage/share/glib-2.0/schemas/"
glib-compile-schemas "$stage/share/glib-2.0/schemas"
rm "$stage"/share/glib-2.0/schemas/*.xml
for theme in Adwaita hicolor; do
    if [[ -d $sysroot/share/icons/$theme ]]; then
        mkdir -p "$stage/share/icons"
        cp -r "$sysroot/share/icons/$theme" "$stage/share/icons/"
    fi
done

rm -f "$out"
(cd "$work" && zip -qr "$out" UnlockInstaller)
echo "Wrote $out ($(find "$stage" -maxdepth 1 -name '*.dll' | wc -l) DLLs)"
