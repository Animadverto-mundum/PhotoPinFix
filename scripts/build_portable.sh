#!/usr/bin/env bash
# Native release build with local source paths remapped out of the executable.
# Source the native HEIC build environment first. No user environment is saved.
set -euo pipefail

project_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
build_options=()
for option in "$@"; do
    case "$option" in
        --offline) build_options+=(--offline) ;;
        *) echo 'Usage: bash scripts/build_portable.sh [--offline]' >&2; exit 2 ;;
    esac
done
command -v cargo >/dev/null
command -v rustc >/dev/null
build_scratch="$(mktemp -d "${TMPDIR:-/tmp}/photopinfix-build.XXXXXX")"
trap 'rm -rf -- "$build_scratch"' EXIT
cargo_registry="${CARGO_HOME:-$HOME/.cargo}"
rust_sysroot="$(rustc --print sysroot)"

# Encoded flags preserve paths containing spaces and avoid shell evaluation.
flags="${CARGO_ENCODED_RUSTFLAGS:-}"
append_flag() {
    if [[ -n "$flags" ]]; then flags+=$'\x1f'; fi
    flags+="$1"
}
if [[ -n "${RUSTFLAGS:-}" && -z "$flags" ]]; then
    # Ordinary RUSTFLAGS uses whitespace-separated options, as Cargo does.
    read -r -a inherited_flags <<< "$RUSTFLAGS"
    for flag in "${inherited_flags[@]}"; do append_flag "$flag"; done
fi
append_flag "--remap-path-prefix=$project_root=/PhotoPinFix"
append_flag "--remap-path-prefix=$cargo_registry=/cargo"
append_flag "--remap-path-prefix=$rust_sysroot=/rust"
append_flag "--remap-path-prefix=$build_scratch=/build"
export CARGO_ENCODED_RUSTFLAGS="$flags"
unset RUSTFLAGS
export CARGO_TARGET_DIR="$build_scratch/target"
export CARGO_PROFILE_RELEASE_STRIP=symbols
export PCRE2_SYS_STATIC=1

cd "$project_root"
cargo build --release --locked --bin photopinfix "${build_options[@]}"
mkdir -p bin
install -m 755 "$CARGO_TARGET_DIR/release/photopinfix" bin/photopinfix
bin/photopinfix --version
printf 'Portable executable: bin/photopinfix\n'
