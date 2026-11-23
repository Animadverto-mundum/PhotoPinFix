#!/usr/bin/env bash
# Build-time only. The resulting application does not invoke this script or
# require an installed HEIF library, Python environment, or codec plugin.
set -euo pipefail

project_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
native_root="${PHOTOPINFIX_NATIVE_ROOT:-$project_root/tools/native-heif}"
build_jobs="${PHOTOPINFIX_BUILD_JOBS:-4}"
mkdir -p "$native_root/downloads" "$native_root/sources" "$native_root/build" "$native_root/install"
native_root="$(cd -- "$native_root" && pwd)"
native_prefix="$native_root/install"

for required in cmake curl tar pkg-config patch; do
    command -v "$required" >/dev/null || { echo "Missing build tool: $required" >&2; exit 1; }
done

# Hashes are the SHA-512 archive pins from Microsoft's vcpkg ports for these
# exact upstream releases. Upstream libraries and their license files remain
# available in sources/ for reproducible builds and source redistribution.
fetch_source() {
    local package="$1" version="$2" expected="$3"
    local archive="$native_root/downloads/$package-$version.tar.gz"
    local source="$native_root/sources/$package-$version"
    if [[ ! -f "$archive" ]]; then
        curl --fail --location --retry 3 --connect-timeout 20 --max-time 300 \
            "https://github.com/strukturag/$package/archive/refs/tags/v$version.tar.gz" \
            --output "$archive.download"
        mv -- "$archive.download" "$archive"
    fi
    local actual
    actual="$(cmake -E sha512sum "$archive")"
    actual="${actual%% *}"
    if [[ "$actual" != "$expected" ]]; then
        echo "SHA-512 mismatch: $archive (expected $expected; got $actual)" >&2
        exit 1
    fi
    if [[ ! -f "$source/CMakeLists.txt" ]]; then
        tar -xzf "$archive" -C "$native_root/sources"
    fi
}

fetch_source libde265 1.1.3 58eabdac466c0b59e0bc82e4d2b6d4fc9cbf6ee0af9562e36d63c0601c92c2fe21bd3dfca2a119934ee4a9dcc73e7207f6ea25dba78382752e901374896c4471
fetch_source libheif 1.23.7 e3b59c0ab6da9a86273d3f6de693d5d1fc955c84d97ca5514c58c62b6ff8ece44e81e215a126d6dbb4248b1e344f60251443c520058033fd71a7282263492180

# Retain the current upstream release and its archive hash. This isolated patch
# replaces two C++20 defaulted comparisons with the identical member comparison
# for GCC 9; it changes no parsing, decoding, bounds checking or security limit.
comparison_patch="$project_root/scripts/patches/libheif-1.23.7-gcc9-comparison.patch"
if ! patch --dry-run --reverse --silent -p1 -d "$native_root/sources/libheif-1.23.7" < "$comparison_patch" >/dev/null 2>&1; then
    patch --forward --batch -p1 -d "$native_root/sources/libheif-1.23.7" < "$comparison_patch"
fi

cmake -S "$native_root/sources/libde265-1.1.3" -B "$native_root/build/libde265" \
    -DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_PREFIX="$native_prefix" \
    -DCMAKE_INSTALL_LIBDIR=lib -DBUILD_SHARED_LIBS=OFF \
    -DENABLE_DECODER=OFF -DENABLE_ENCODER=OFF -DENABLE_SDL=OFF \
    -DENABLE_SHERLOCK265=OFF -DENABLE_INTERNAL_DEVELOPMENT_TOOLS=OFF \
    -DWITH_FUZZERS=OFF -DDE265_LOG_LEVEL=none
cmake --build "$native_root/build/libde265" --parallel "$build_jobs"
cmake --install "$native_root/build/libde265" --config Release

export PKG_CONFIG_PATH="$native_prefix/lib/pkgconfig${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}"
cmake -S "$native_root/sources/libheif-1.23.7" -B "$native_root/build/libheif" \
    -DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_PREFIX="$native_prefix" \
    -DCMAKE_INSTALL_LIBDIR=lib -DCMAKE_PREFIX_PATH="$native_prefix" \
    -DBUILD_SHARED_LIBS=OFF -DBUILD_TESTING=OFF -DBUILD_DOCUMENTATION=OFF \
    -DWITH_EXAMPLES=OFF -DWITH_EXAMPLE_HEIF_THUMB=OFF -DWITH_EXAMPLE_HEIF_VIEW=OFF \
    -DWITH_GDK_PIXBUF=OFF -DENABLE_PLUGIN_LOADING=OFF \
    -DWITH_LIBDE265=ON -DWITH_LIBDE265_PLUGIN=OFF \
    -DLIBDE265_INCLUDE_DIR="$native_prefix/include" \
    -DLIBDE265_LIBRARY="$native_prefix/lib/libde265.a" \
    -DWITH_X265=OFF -DWITH_X264=OFF -DWITH_KVAZAAR=OFF -DWITH_UVG266=OFF \
    -DWITH_VVENC=OFF -DWITH_VVDEC=OFF -DWITH_OpenH264_DECODER=OFF \
    -DWITH_AOM_DECODER=OFF -DWITH_AOM_ENCODER=OFF -DWITH_DAV1D=OFF \
    -DWITH_SvtEnc=OFF -DWITH_RAV1E=OFF -DWITH_JPEG_DECODER=OFF -DWITH_JPEG_ENCODER=OFF \
    -DWITH_OpenJPEG_DECODER=OFF -DWITH_OpenJPEG_ENCODER=OFF \
    -DWITH_OPENJPH_ENCODER=OFF -DWITH_OPENJPH_DECODER=OFF -DWITH_FFMPEG_DECODER=OFF \
    -DWITH_WEBCODECS=OFF -DWITH_UNCOMPRESSED_CODEC=OFF -DWITH_HEADER_COMPRESSION=OFF \
    -DWITH_LIBSHARPYUV=OFF
cmake --build "$native_root/build/libheif" --parallel "$build_jobs"
cmake --install "$native_root/build/libheif" --config Release
cmake -DNATIVE_PREFIX="$native_prefix" -P "$project_root/scripts/relocate_native_install.cmake"

test -s "$native_prefix/lib/libheif.a"
test -s "$native_prefix/lib/libde265.a"
mkdir -p "$native_prefix/share/licenses"
cp "$native_root/sources/libheif-1.23.7/COPYING" "$native_prefix/share/licenses/libheif-COPYING"
cp "$native_root/sources/libde265-1.1.3/COPYING" "$native_prefix/share/licenses/libde265-COPYING"
cat > "$native_root/env.sh" <<'SHELL'
# Generated build environment; source from Bash before cargo builds.
photopinfix_native="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
export PKG_CONFIG_PATH="$photopinfix_native/install/lib/pkgconfig${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}"
export LIBHEIF_STATIC=1 SYSTEM_DEPS_LINK=static PCRE2_SYS_STATIC=1
unset photopinfix_native
SHELL
pkg-config --modversion libheif libde265
pkg-config --libs --static libheif
printf 'Native HEIC dependencies built. Run: source %q\n' "$native_root/env.sh"
