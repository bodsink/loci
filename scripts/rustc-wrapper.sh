#!/bin/sh
# Cargo rustc-wrapper: run rustc, then copy the release `loci` binary to
# ~/.local/bin/loci so `cargo build --release` updates the command on PATH.
#
# Cargo invokes this as: rustc-wrapper <rustc> <args...>
# It does not pass `-o target/release/loci`. The bin is emitted as
# `<out-dir>/loci<extra-filename>` under `release/deps`; cargo then copies
# that file to `target/release/loci`. We copy the same file to PATH.
#
# A failed compile must stay a failed compile. Debug builds, tests, and
# crates in release/deps other than the `loci` bin are left alone.

rustc="$1"
shift

out=""
out_dir=""
crate_name=""
crate_type=""
extra_filename=""
is_test=0
prev=""

for arg in "$@"; do
    if [ "$prev" = "-o" ]; then
        out="$arg"
        prev=""
        continue
    fi
    if [ "$prev" = "--out-dir" ]; then
        out_dir="$arg"
        prev=""
        continue
    fi
    if [ "$prev" = "--crate-name" ]; then
        crate_name="$arg"
        prev=""
        continue
    fi
    if [ "$prev" = "--crate-type" ]; then
        crate_type="$arg"
        prev=""
        continue
    fi
    if [ "$prev" = "-C" ]; then
        case "$arg" in
            extra-filename=*) extra_filename="${arg#extra-filename=}" ;;
        esac
        prev=""
        continue
    fi
    case "$arg" in
        --test) is_test=1 ;;
        -o) prev="-o" ;;
        --out-dir) prev="--out-dir" ;;
        --crate-name) prev="--crate-name" ;;
        --crate-type) prev="--crate-type" ;;
        -C) prev="-C" ;;
        -o?*) out="${arg#-o}" ;;
        --out-dir=*) out_dir="${arg#--out-dir=}" ;;
        --crate-name=*) crate_name="${arg#--crate-name=}" ;;
        --crate-type=*) crate_type="${arg#--crate-type=}" ;;
        -Cextra-filename=*) extra_filename="${arg#-Cextra-filename=}" ;;
    esac
done

"$rustc" "$@"
status=$?
if [ "$status" -ne 0 ]; then
    exit "$status"
fi

if [ -n "${LOCI_SKIP_RELEASE_INSTALL:-}" ] || [ -z "${HOME:-}" ]; then
    exit 0
fi

if [ "$is_test" -eq 1 ]; then
    exit 0
fi

# Prefer an explicit -o (used by tests and by rustc when it is given one).
# Otherwise rebuild the path cargo actually uses for the loci bin.
if [ -z "$out" ] && [ "$crate_name" = "loci" ] && [ "$crate_type" = "bin" ]; then
    out="${out_dir}/loci${extra_filename}"
fi

case "$out" in
    */release/loci | */release/loci.exe) ;;
    */release/deps/loci | */release/deps/loci.exe) ;;
    */release/deps/loci-*) ;;
    *) exit 0 ;;
esac

# rustc also writes `loci-<hash>.d` next to the binary; do not install that.
case "$out" in
    *.d | *.rlib | *.rmeta) exit 0 ;;
esac

if [ ! -f "$out" ]; then
    if [ -f "${out}.exe" ]; then
        out="${out}.exe"
    else
        exit 0
    fi
fi

dest="$HOME/.local/bin/loci"
if [ "$out" = "$dest" ]; then
    exit 0
fi

mkdir -p "$HOME/.local/bin" || exit 0
tmp="$dest.loci-tmp"
# Same dance as `loci install`: copy aside, then rename, so replacing a
# running binary does not hit ETXTBSY.
if cp -f "$out" "$tmp" && chmod 755 "$tmp" && mv -f "$tmp" "$dest"; then
    exit 0
fi
rm -f "$tmp"
exit 0
