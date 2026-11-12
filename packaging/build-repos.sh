#!/usr/bin/env bash
# Turn a folder of packages into signed dnf and apt repositories.
#
#   packaging/build-repos.sh <site-dir> <base-url> <gpg-key-id>
#
# Expects the packages in <site-dir>/rpm/<fedora>/*.rpm (one folder per Fedora
# release, e.g. rpm/43 and rpm/44) and <site-dir>/apt/*.deb and the signing key
# in the current GnuPG keyring. <base-url> is where <site-dir> will be served
# from. Needs: createrepo_c, dpkg-scanpackages, apt-ftparchive, gpg.
set -euo pipefail

site=$(realpath "$1")
base=${2%/}
key=$3
sign=(gpg --batch --yes --local-user "$key")

# The public key, armored (dnf) and binary (apt's signed-by).
gpg --batch --yes --armor --export "$key" > "$site/key.asc"
gpg --batch --yes --export "$key" > "$site/key.gpg"

# --- dnf --------------------------------------------------------------------
# One repository per Fedora release: each package is built against that
# release's Qt, so the releases must not share a repository.
for repo in "$site"/rpm/*/; do
    createrepo_c --quiet "$repo"
    "${sign[@]}" --detach-sign --armor --output "$repo/repodata/repomd.xml.asc" "$repo/repodata/repomd.xml"
done
# dnf fills in $releasever with the Fedora release, so one .repo file serves both.
cat > "$site/rpm/unlockinstaller.repo" <<REPO
[unlockinstaller]
name=UnlockInstaller
baseurl=$base/rpm/\$releasever
enabled=1
# The repository metadata is signed (and lists each package's checksum);
# the packages themselves are not.
gpgcheck=0
repo_gpgcheck=1
gpgkey=$base/key.asc
REPO

# --- apt (flat repository) --------------------------------------------------
(
    cd "$site/apt"
    dpkg-scanpackages --multiversion . > Packages
    gzip -9 --keep --force Packages
    apt-ftparchive \
        -o APT::FTPArchive::Release::Origin=UnlockInstaller \
        -o APT::FTPArchive::Release::Label=UnlockInstaller \
        release . > Release
    "${sign[@]}" --clearsign --output InRelease Release
    "${sign[@]}" --detach-sign --armor --output Release.gpg Release
)

# --- landing page -----------------------------------------------------------
cat > "$site/index.html" <<HTML
<!doctype html>
<html lang="en">
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>UnlockInstaller package repositories</title>
<style>
  body { font: 16px/1.5 system-ui, sans-serif; max-width: 46rem; margin: 2rem auto; padding: 0 1rem; }
  pre { background: #8881; padding: .8rem; border-radius: 6px; overflow-x: auto; }
</style>
<h1>UnlockInstaller package repositories</h1>
<h2>Fedora (dnf)</h2>
<pre>sudo curl -fsSLo /etc/yum.repos.d/unlockinstaller.repo $base/rpm/unlockinstaller.repo
sudo dnf install unlockinstaller</pre>
<h2>Debian / Ubuntu (apt)</h2>
<pre>sudo curl -fsSLo /usr/share/keyrings/unlockinstaller.gpg $base/key.gpg
echo "deb [signed-by=/usr/share/keyrings/unlockinstaller.gpg] $base/apt ./" | sudo tee /etc/apt/sources.list.d/unlockinstaller.list
sudo apt update
sudo apt install unlockinstaller</pre>
<p>Packages are signed with <a href="key.asc">this key</a>.</p>
</html>
HTML
echo "Built repositories in $site for $base"
