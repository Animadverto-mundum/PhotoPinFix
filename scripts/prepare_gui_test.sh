#!/usr/bin/env bash
# Developer-only local setup. Does not install or change system packages.
set -euo pipefail
project_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
font_root="$project_root/assets/fonts"
test_root="$project_root/tools/gui-test"
mkdir -p "$font_root" "$test_root/downloads" "$test_root/root"
font_base='https://raw.githubusercontent.com/notofonts/noto-cjk/Sans2.004/Sans'
if [[ ! -s "$font_root/NotoSansSC-Regular.otf" ]]; then
    curl --fail --location --retry 3 --connect-timeout 20 --max-time 300 \
        "$font_base/SubsetOTF/SC/NotoSansSC-Regular.otf" \
        --output "$font_root/NotoSansSC-Regular.otf.download"
    mv "$font_root/NotoSansSC-Regular.otf.download" "$font_root/NotoSansSC-Regular.otf"
fi
curl --fail --location --retry 3 --connect-timeout 20 --max-time 120 \
    'https://raw.githubusercontent.com/notofonts/noto-cjk/main/Sans/LICENSE' --output "$font_root/OFL.txt"
(
    cd "$font_root"
    cmake -E sha256sum NotoSansSC-Regular.otf OFL.txt > SHA256SUMS
)
if [[ ! -x "$test_root/root/usr/bin/Xvfb" ]]; then
    if ! (
        cd "$test_root/downloads"
        apt-get download xvfb
    ); then
        # A stale focal apt cache may point to .17 after Ubuntu has replaced it
        # with .20. This known fallback is deliberately limited to this ABI and
        # architecture; other distributions should refresh their own apt cache.
        . /etc/os-release
        if [[ "${ID:-}" != ubuntu || "${VERSION_ID:-}" != 20.04 || "$(dpkg --print-architecture)" != amd64 ]]; then
            echo 'Could not download Xvfb; refresh the package index or install Xvfb for this distribution.' >&2
            exit 1
        fi
        archive="$test_root/downloads/xvfb_1.20.13-1ubuntu1~20.04.20_amd64.deb"
        curl --fail --location --retry 3 --connect-timeout 20 --max-time 120 \
            'https://archive.ubuntu.com/ubuntu/pool/universe/x/xorg-server/xvfb_1.20.13-1ubuntu1~20.04.20_amd64.deb' \
            --output "$archive.download"
        printf '%s  %s\n' 'b671759ad2b8280723b0b55361368dc69c12cb301ea9a6a5e443e4f8d2745a2d' "$archive.download" | sha256sum --check --status
        mv "$archive.download" "$archive"
    fi
    for archive in "$test_root/downloads"/xvfb_*.deb; do
        dpkg-deb --extract "$archive" "$test_root/root"
    done
fi
ldd "$test_root/root/usr/bin/Xvfb" > "$test_root/xvfb-libraries.txt"
if grep -q 'not found' "$test_root/xvfb-libraries.txt"; then
    cat "$test_root/xvfb-libraries.txt" >&2
    echo 'Xvfb has missing system libraries; local additional package extraction is needed.' >&2
    exit 1
fi
printf 'Font ready: %s\nXvfb ready: %s\n' "$font_root/NotoSansSC-Regular.otf" "$test_root/root/usr/bin/Xvfb"
