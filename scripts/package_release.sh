#!/usr/bin/env bash
# Assemble a reviewable development artifact. This script never publishes it.
# Invoke only after building/testing the executable for the named target.
set -euo pipefail

project_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
target_name="${1:?usage: bash scripts/package_release.sh TARGET [BINARY]}"
case "$target_name" in
    *-windows-msvc) executable_name=photopinfix.exe ;;
    *) executable_name=photopinfix ;;
esac
binary_path="${2:-$project_root/target/$target_name/release/$executable_name}"
test -s "$binary_path" || { echo "Built executable missing: $binary_path" >&2; exit 1; }
artifact_root="$project_root/target/artifacts"
mkdir -p "$artifact_root"
staging="$(mktemp -d "$artifact_root/.package.XXXXXX")"
trap 'rm -rf -- "$staging"' EXIT
package_name="photopinfix-$target_name-development"
bundle="$staging/$package_name"
source_root="$bundle/source"
mkdir -p "$source_root" "$bundle/licenses" "$bundle/relink"
cp "$binary_path" "$bundle/$executable_name"
cp "$project_root/THIRD_PARTY_NOTICES.md" "$bundle/THIRD_PARTY_NOTICES.md"

# Explicit source allowlist: no photos, local credentials, tool installations,
# environment registration, or arbitrary working-directory contents are copied.
for filename in Cargo.toml Cargo.lock build.rs rust-toolchain.toml README.md THIRD_PARTY_NOTICES.md LICENSE LICENSE.md LICENSE.txt COPYING NOTICE .gitignore; do
    if [[ -f "$project_root/$filename" ]]; then cp "$project_root/$filename" "$source_root/"; fi
done
for directory in src assets tests scripts .github; do
    if [[ -d "$project_root/$directory" ]]; then
        tar -C "$project_root" --exclude=__pycache__ --exclude='*.pyc' -cf - "$directory" | tar -C "$source_root" -xf -
    fi
done
mkdir -p "$source_root/.cargo" "$source_root/tools/native-heif"
# The vendor directory and relative Cargo config make the Rust source closure
# reusable after extraction. This is a build-time operation, not part of the app.
(
    cd "$source_root"
    cargo vendor --locked --versioned-dirs vendor > .cargo/config.toml
)
(
    rustc -Vv
    cargo -V
    cmake --version
    # Arbitrary environment values (notably path-remap RUSTFLAGS) may contain
    # account names and local paths. Record build choices, never the environment.
    printf 'Target: %s\nPCRE2 static: enabled\nNative codecs: static\n' "$target_name"
) > "$bundle/build-toolchain.txt"

native_root="${PHOTOPINFIX_NATIVE_ROOT:-$project_root/tools/native-heif}"
if [[ "$target_name" == *-windows-msvc ]]; then
    triplet="${VCPKGRS_TRIPLET:?source tools/native-heif/env.ps1 before packaging}"
    vcpkg_root="${VCPKG_ROOT:?VCPKG_ROOT is missing}"
    # Git Bash understands /c/...; native environment values may use C:\... .
    if command -v cygpath >/dev/null; then vcpkg_root="$(cygpath -u "$vcpkg_root")"; fi
    packaged_vcpkg="$source_root/tools/native-heif/vcpkg"
    mkdir -p "$packaged_vcpkg" "$bundle/relink/native-sources" "$bundle/relink/vcpkg-ports"
    cp "$vcpkg_root/.vcpkg-root" "$packaged_vcpkg/"
    cp -R "$vcpkg_root/installed" "$packaged_vcpkg/installed"
    for library in libheif libde265; do
        cp -R "$vcpkg_root/buildtrees/$library/src" "$bundle/relink/native-sources/$library"
        cp -R "$vcpkg_root/ports/$library" "$bundle/relink/vcpkg-ports/$library"
        cp "$vcpkg_root/installed/$triplet/share/$library/copyright" "$bundle/licenses/$library-COPYING"
    done
    # No native shared libraries should be needed, including the MSVC CRT.
    command -v dumpbin >/dev/null || { echo 'dumpbin is required for Windows dependency verification' >&2; exit 1; }
    (cd "$bundle"; MSYS_NO_PATHCONV=1 dumpbin /DEPENDENTS "$executable_name") > "$bundle/native-dependencies.txt"
    if grep -Eiq '(heif|de265|pcre2|python|perl|vcruntime|msvcp)[^[:space:]]*\.dll' "$bundle/native-dependencies.txt"; then
        echo 'Executable unexpectedly depends on an external codec/interpreter/CRT DLL' >&2
        exit 1
    fi
    cat > "$source_root/relink-env.ps1" <<'POWERSHELL'
$env:VCPKG_ROOT = Join-Path $PSScriptRoot 'tools/native-heif/vcpkg'
$env:PCRE2_SYS_STATIC = '1'
$env:RUSTFLAGS = (($env:RUSTFLAGS + ' -C target-feature=+crt-static').Trim())
Remove-Item Env:VCPKGRS_DYNAMIC -ErrorAction SilentlyContinue
POWERSHELL
    printf "\$env:VCPKGRS_TRIPLET = '%s'\n" "$triplet" >> "$source_root/relink-env.ps1"
else
    test -s "$native_root/install/lib/libheif.a"
    test -s "$native_root/install/lib/libde265.a"
    cp -R "$native_root/install" "$source_root/tools/native-heif/install"
    cp -R "$native_root/sources" "$source_root/tools/native-heif/sources"
    cp -R "$native_root/downloads" "$source_root/tools/native-heif/downloads"
    cp "$native_root/install/share/licenses/"* "$bundle/licenses/"
    cmake -DNATIVE_PREFIX="$source_root/tools/native-heif/install" \
        -P "$project_root/scripts/relocate_native_install.cmake"
    cat > "$source_root/relink-env.sh" <<'SHELL'
photopinfix_source="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
export PKG_CONFIG_PATH="$photopinfix_source/tools/native-heif/install/lib/pkgconfig${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}"
export LIBHEIF_STATIC=1 SYSTEM_DEPS_LINK=static PCRE2_SYS_STATIC=1
SHELL
    if [[ -n "${MACOSX_DEPLOYMENT_TARGET:-}" ]]; then
        printf 'export MACOSX_DEPLOYMENT_TARGET=%q\n' "$MACOSX_DEPLOYMENT_TARGET" >> "$source_root/relink-env.sh"
    fi
    case "$target_name" in
        *-apple-darwin) (cd "$bundle"; otool -L "./$executable_name") > "$bundle/native-dependencies.txt" ;;
        *-linux-gnu) (cd "$bundle"; ldd "./$executable_name") > "$bundle/native-dependencies.txt" ;;
        *) echo "Unsupported packaging target: $target_name" >&2; exit 1 ;;
    esac
    if grep -Eiq '(libheif|libde265|libpcre2|libpython|libperl)' "$bundle/native-dependencies.txt"; then
        echo 'Executable unexpectedly depends on a codec/interpreter shared library' >&2
        exit 1
    fi
fi

# Include each Rust crate's own notices; complete original files also remain in
# source/vendor, including native PCRE2's source and notices inside pcre2-sys.
for dependency in "$source_root/vendor/"*; do
    [[ -d "$dependency" ]] || continue
    destination="$bundle/licenses/$(basename "$dependency")"
    for notice in "$dependency"/LICENSE* "$dependency"/LICENCE* "$dependency"/COPYING* "$dependency"/NOTICE*; do
        if [[ -f "$notice" ]]; then mkdir -p "$destination"; cp "$notice" "$destination/"; fi
    done
done
cat > "$bundle/RELINKING.md" <<EOF
# Rebuilding or relinking this development build

Target: \`$target_name\`. Exact build tools are recorded in \`build-toolchain.txt\`.
The \`source/\` directory contains the application, Cargo.lock, vendored Rust
dependencies, native static libraries, original notices and build scripts.
Python and Perl are not required to compile or run this application.

On Linux/macOS, enter \`source/\`, run \`source relink-env.sh\`, then run
\`cargo build --offline --locked --release --target $target_name\`.
To modify libheif/libde265, edit their supplied sources under
\`tools/native-heif/sources/\`, run \`bash scripts/build_native_heif.sh\`, source
\`tools/native-heif/env.sh\`, then rebuild the Rust executable. Original archives
remain hash-pinned; the GCC 9 compatibility change is a separate patch.

On Windows, enter \`source/\` in a Visual Studio developer PowerShell, run
\`. ./relink-env.ps1\`, then the same Cargo build command above. This uses the
included vcpkg static installation, without needing the vcpkg executable.
Applied native sources and port patches are under \`../relink/\`. Replace the
matching static .lib files in the supplied vcpkg installation after rebuilding
a modified library, and run \`cargo clean -p libheif-sys\` before the Cargo build.
The source build script can also recreate the pinned vcpkg toolchain installation.

No executable signing, installation lock, or external runtime helper is applied.
System compiler/linker/SDK tools and Rust are build prerequisites. Development
fixture generators and independent oracle tests may use optional Python/Perl.
See THIRD_PARTY_NOTICES.md and the complete licenses for applicable terms.
EOF
printf '%s\n' 'Development/test artifact. No formal release is published by this workflow.' \
    'PhotoPinFix application licensing has not been selected by these scripts.' \
    'Keep source, notices and relinking material with this artifact.' > "$bundle/DEVELOPMENT_ARTIFACT.txt"
(
    cd "$bundle"
    PATH='' "./$executable_name" --version
    PATH='' "./$executable_name" --help > /dev/null
    cmake -E sha256sum "$executable_name" source/Cargo.lock
) > "$bundle/checksums-and-smoke.txt"
# Archive ownership is reproducible and does not disclose the packaging user.
if tar --version | head -n 1 | grep -q 'GNU tar'; then
    tar --owner=0 --group=0 --numeric-owner -C "$staging" -czf "$artifact_root/$package_name.tar.gz" "$package_name"
else
    tar --uid 0 --gid 0 --uname root --gname root -C "$staging" -czf "$artifact_root/$package_name.tar.gz" "$package_name"
fi
(
    cd "$artifact_root"
    cmake -E sha256sum "$package_name.tar.gz" > "$package_name.tar.gz.sha256"
)
printf 'Development artifact ready: %s/%s.tar.gz\n' "$artifact_root" "$package_name"
