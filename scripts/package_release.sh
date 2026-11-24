#!/usr/bin/env bash
# Assemble paired application and corresponding-source artifacts; never publish.
# Keep both archives available together when distributing this build.
# Invoke only after building/testing the executable for the named target.
set -euo pipefail

project_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
target_name="${1:?usage: bash scripts/package_release.sh TARGET [BINARY]}"
case "$target_name" in
    x86_64-pc-windows-msvc|aarch64-pc-windows-msvc) executable_name=photopinfix.exe ;;
    x86_64-unknown-linux-gnu|aarch64-unknown-linux-gnu|x86_64-apple-darwin|aarch64-apple-darwin) executable_name=photopinfix ;;
    *) echo 'Unsupported packaging target' >&2; exit 1 ;;
esac
binary_path="${2:-$project_root/target/$target_name/release/$executable_name}"
test -s "$binary_path" || { echo "Built executable missing: $binary_path" >&2; exit 1; }
artifact_root="$project_root/target/artifacts"
mkdir -p "$artifact_root"
staging="$(mktemp -d "$artifact_root/.package.XXXXXX")"
trap 'rm -rf -- "$staging"' EXIT
app_name="photopinfix-$target_name-app"
source_name="photopinfix-$target_name-source"
app_bundle="$staging/$app_name"
bundle="$staging/$source_name"
source_root="$bundle/source"
mkdir -p "$source_root" "$bundle/licenses" "$bundle/relink" "$app_bundle"
cp "$binary_path" "$app_bundle/$executable_name"
cp "$project_root/THIRD_PARTY_NOTICES.md" "$bundle/THIRD_PARTY_NOTICES.md"
cp "$project_root/THIRD_PARTY_NOTICES.md" "$app_bundle/THIRD_PARTY_NOTICES.md"

# Explicit source allowlist: no photos, local credentials, tool installations,
# environment registration, or arbitrary working-directory contents are copied.
for filename in Cargo.toml Cargo.lock build.rs rust-toolchain.toml README.md THIRD_PARTY_NOTICES.md LICENSE LICENSE.md LICENSE.txt COPYING NOTICE .gitignore; do
    if [[ -f "$project_root/$filename" ]]; then cp "$project_root/$filename" "$source_root/"; fi
done
for directory in src assets tests scripts examples docs .github; do
    if [[ -d "$project_root/$directory" ]]; then
        tar -C "$project_root" --exclude=__pycache__ --exclude='*.pyc' -cf - "$directory" | tar -C "$source_root" -xf -
    fi
done
# Keep public fixture descriptions and expected results, never downloaded
# images, RAW files, user-created manifests, or arbitrary demo contents.
for relative in demo/README.md demo/expected.json demo/.photopinfix-demo.json \
    demo/raw/README.md demo/raw/manifest.json demo/raw/expected.json demo/raw-sources/catalog.json; do
    if [[ -f "$project_root/$relative" ]]; then
        mkdir -p "$source_root/$(dirname "$relative")"
        cp "$project_root/$relative" "$source_root/$relative"
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
    (cd "$app_bundle"; MSYS_NO_PATHCONV=1 dumpbin /DEPENDENTS "$executable_name") > "$bundle/native-dependencies.txt"
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
        *-apple-darwin) (cd "$app_bundle"; otool -L "./$executable_name") > "$bundle/native-dependencies.txt" ;;
        *-linux-gnu) (cd "$app_bundle"; ldd "./$executable_name") > "$bundle/native-dependencies.txt" ;;
        *) echo "Unsupported packaging target: $target_name" >&2; exit 1 ;;
    esac
    if grep -Eiq '(libheif|libde265|libpcre2|libpython|libperl)' "$bundle/native-dependencies.txt"; then
        echo 'Executable unexpectedly depends on a codec/interpreter shared library' >&2
        exit 1
    fi
fi

# Include nested dependency notices as well as crate root files (for example,
# PCRE2's bundled C source). Preserve relative paths to avoid filename collisions.
copy_notices() {
    local notice_root="$1" destination="$2" notice relative
    [[ -d "$notice_root" ]] || return 0
    while IFS= read -r -d '' notice; do
        relative="${notice#"$notice_root/"}"
        mkdir -p "$destination/$(dirname "$relative")"
        cp "$notice" "$destination/$relative"
    done < <(find "$notice_root" -type f \
        \( -iname 'LICENSE*' -o -iname 'LICENCE*' -o -iname 'COPYING*' \
        -o -iname 'NOTICE*' -o -iname 'COPYRIGHT*' -o -iname 'OFL*' \
        -o -ipath '*/licenses/*' -o -ipath '*/licences/*' \) -print0)
}
copy_notices "$source_root/vendor" "$bundle/licenses/rust"
copy_notices "$source_root/tools/native-heif/sources" "$bundle/licenses/native"
copy_notices "$bundle/relink/native-sources" "$bundle/licenses/native"
mkdir -p "$bundle/licenses/NotoSansSC"
cp "$project_root/assets/fonts/OFL.txt" "$bundle/licenses/NotoSansSC/OFL.txt"
for notice in LICENSE LICENSE.md LICENSE.txt COPYING NOTICE; do
    if [[ -f "$project_root/$notice" ]]; then cp "$project_root/$notice" "$bundle/licenses/PhotoPinFix-$notice"; fi
done
cp -R "$bundle/licenses" "$app_bundle/licenses"
cat > "$bundle/RELINKING.md" <<EOF
# Rebuilding or relinking the paired PhotoPinFix application

Application archive: \`$app_name.tar.gz\`.
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
# Bind both packages to the same executable and application source snapshot.
# Record no environment variables, personal names, remote URLs, or local paths.
commit=unknown
source_state=unknown
run_url=not-recorded
if [[ "${GITHUB_RUN_ID:-}" =~ ^[0-9]+$ ]]; then
    run_url="https://github.com/Animadverto-mundum/PhotoPinFix/actions/runs/$GITHUB_RUN_ID"
fi
if command -v git >/dev/null && git -C "$project_root" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
    candidate="$(git -C "$project_root" rev-parse HEAD)"
    if [[ "$candidate" =~ ^[0-9a-fA-F]{40,64}$ ]]; then commit="$candidate"; fi
    if [[ -z "$(git -C "$project_root" status --porcelain --untracked-files=normal)" ]]; then
        source_state=clean
    else
        source_state=modified
    fi
fi
binary_checksum="$(cd "$app_bundle" && cmake -E sha256sum "$executable_name")"
binary_checksum="${binary_checksum%% *}"
lock_checksum="$(cd "$source_root" && cmake -E sha256sum Cargo.lock)"
lock_checksum="${lock_checksum%% *}"
cat > "$bundle/PROVENANCE.txt" <<EOF
PhotoPinFix paired build artifacts
Target: $target_name
Application archive: $app_name.tar.gz
Corresponding source archive: $source_name.tar.gz
Application binary SHA-256: $binary_checksum
Cargo.lock SHA-256: $lock_checksum
Source Git commit: $commit
Source checkout state at packaging: $source_state
GitHub Actions run: $run_url
Exact packaged sources: source/ (see SHA256SUMS)
EOF
cp "$bundle/PROVENANCE.txt" "$app_bundle/PROVENANCE.txt"
(
    cd "$app_bundle"
    PATH='' "./$executable_name" --version
    PATH='' "./$executable_name" --help > /dev/null
) > "$bundle/runtime-smoke.txt"

# Archive ownership does not disclose the packaging user. GNU tar is used on
# Linux/Git Bash; macOS's bsdtar supports the explicit uid/gid/name flags.
archive_bundle() {
    local name="$1"
    cmake -DPACKAGE_ROOT="$staging/$name" -P "$project_root/scripts/package_checksums.cmake"
    if tar --version | head -n 1 | grep -q 'GNU tar'; then
        tar --owner=0 --group=0 --numeric-owner -C "$staging" -czf "$artifact_root/$name.tar.gz" "$name"
    else
        tar --uid 0 --gid 0 --uname root --gname root -C "$staging" -czf "$artifact_root/$name.tar.gz" "$name"
    fi
    (cd "$artifact_root" && cmake -E sha256sum "$name.tar.gz" > "$name.tar.gz.sha256")
}
archive_bundle "$source_name"
source_checksum="$(cat "$artifact_root/$source_name.tar.gz.sha256")"
source_checksum="${source_checksum%% *}"
cat > "$app_bundle/SOURCE.txt" <<EOF
Corresponding source for this exact application binary

Archive: $source_name.tar.gz
SHA-256: $source_checksum
Download location: the source artifact alongside this app artifact in the same
GitHub Actions run or release. Both artifacts must remain available together.
Build run: $run_url
Repository: https://github.com/Animadverto-mundum/PhotoPinFix
Source Git commit: $commit

The source archive includes application sources, locked/vendored Rust crates,
native library sources and patches, static libraries, licenses and RELINKING.md.
It is sufficient to rebuild or relink this build using the documented compiler,
SDK and Rust prerequisites. No application runtime interpreter is required.
EOF
cat > "$app_bundle/README.txt" <<EOF
PhotoPinFix ($target_name)

Terminal: ./$executable_name [photo-directory]
Desktop:  ./$executable_name --gui [photo-directory]
Windows PowerShell: ./$executable_name --gui [photo-directory]

This one executable contains both TUI and GUI. Python, Perl, ExifTool and
external image converters are not required. Desktop mode uses the operating
system's window system and graphics drivers. Linux supports X11 and Wayland.
Files are changed only after you confirm writing in PhotoPinFix.

SHA256SUMS checks this package's files. SOURCE.txt identifies the exact matching
source/relinking archive and its SHA-256. Keep the notices and licenses with the
application and provide the corresponding source archive when redistributing.
See THIRD_PARTY_NOTICES.md for the bundled components and applicable terms.
EOF
archive_bundle "$app_name"
printf 'Application artifact: %s/%s.tar.gz\nCorresponding source artifact: %s/%s.tar.gz\n' \
    "$artifact_root" "$app_name" "$artifact_root" "$source_name"
