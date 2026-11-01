#!/bin/bash
# FaceHAL/scripts/build.sh
#
# Cross-compiles the AIDL face HAL for aarch64-linux-android using the NDK, then
# verifies the artifact offline. Never touches the device and never repacks.
#
# Usage: scripts/build.sh [NDK_PATH]
set -euo pipefail

HERE="$(dirname "$(dirname "$(readlink -f "${BASH_SOURCE[0]}")")")"
CRATE="$HERE"
NDK="${1:-${ANDROID_NDK_HOME:-/opt/android-sdk/ndk/27.3.13750724}}"

RED=$'\033[1;31m'; GRN=$'\033[1;32m'; YEL=$'\033[1;33m'; NC=$'\033[0m'

# The NDK root and the toolchain dir are both in use on this host; accept either.
if [[ ! -x "$NDK/bin/clang++" ]]; then
    for cand in "$NDK"/toolchains/llvm/prebuilt/*/; do
        [[ -x "${cand}bin/clang++" ]] && NDK="${cand%/}" && break
    done
fi
if [[ ! -x "$NDK/bin/clang++" ]]; then
    echo -e "${RED}no usable NDK (tried \$1, \$ANDROID_NDK_HOME, and the bundled toolchain dirs)${NC}"
    exit 1
fi
echo -e "${GRN}NDK:${NC} $NDK"

# API 30 (Android 11) is the floor for the MT6893 vendor image; the ROM itself
# is Android 15, and a higher target would add a DT_NEEDED the vendor namespace
# cannot satisfy.
export TARGET_CC="$NDK/bin/clang++"
export TARGET_AR="$NDK/bin/llvm-ar"
export CC_aarch64_linux_android="$NDK/bin/aarch64-linux-android30-clang"
export AR_aarch64_linux_android="$NDK/bin/llvm-ar"
export CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER="$NDK/bin/aarch64-linux-android30-clang"
export CARGO_TARGET_AARCH64_LINUX_ANDROID_RUSTFLAGS="-C link-arg=-fuse-ld=lld"

if ! rustup target list --installed | grep -q '^aarch64-linux-android$'; then
    echo -e "${YEL}installing aarch64-linux-android target${NC}"
    rustup target add aarch64-linux-android
fi

cd "$CRATE"
echo -e "${GRN}building...${NC}"
cargo build --release --target aarch64-linux-android --offline

BIN="target/aarch64-linux-android/release/face-hal-service"
[[ -f "$BIN" ]] || { echo -e "${RED}build produced no binary${NC}"; exit 1; }

echo
echo -e "${GRN}artifact${NC} $BIN ($(stat -c%s "$BIN") bytes)"
file "$BIN" | sed 's/^/  /'
echo -e "${GRN}DT_NEEDED${NC}"
readelf -d "$BIN" | grep NEEDED | sed 's/^/  /'
echo -e "${GRN}UND symbols from our own libs${NC}"
readelf --dyn-syms "$BIN" | awk '$7=="UND" && $8 ~ /^st_/ {print "  "$8}' | sort -u

echo
# The ROM tree is a sibling checkout, not part of this repository. Skip the
# resolution check when it is absent rather than failing a build that is fine.
ROM_TREE="${ROM_TREE:-$HERE/../../erofs}"
if [[ ! -d "$ROM_TREE" ]]; then
    echo -e "${YEL}no ROM tree at $ROM_TREE, skipping the DT_NEEDED resolution check${NC}"
    echo "  set ROM_TREE to check the libraries against an unpacked tree"
    exit 0
fi

echo -e "${GRN}verifying every DT_NEEDED resolves in the ROM tree...${NC}"
# Bionic core libs ship in /apex/com.android.runtime (not in any partition dir
# of the unpacked tree) and are provided by the linker itself, so they are
# always resolvable and are not checked against the tree.
BIONIC_CORE="^(libc\.so|libdl\.so|libm\.so|libc\+\+\.so|libc\+\+_shared\.so|libdl_android\.so|libnativehelper\.so)$"
fail=0
while read -r lib; do
    if [[ "$lib" =~ $BIONIC_CORE ]]; then
        echo "  OK   $lib (bionic core / runtime apex)"
    elif [[ -f "$ROM_TREE/vendor/lib64/$lib" ]]; then
        echo "  OK   $lib (vendor/lib64)"
    elif [[ -f "$ROM_TREE/system/system/lib64/$lib" ]]; then
        if grep -qx "$lib" "$ROM_TREE/system/system/etc/llndk.libraries.txt" 2>/dev/null; then
            echo "  OK   $lib (llndk)"
        else
            echo "  WARN $lib present in /system/lib64 but NOT llndk — vendor namespace may not resolve it"
        fi
    else
        echo "  FAIL $lib not found anywhere in the ROM tree"
        fail=1
    fi
done < <(readelf -d "$BIN" | awk '/NEEDED/ {gsub(/[][]/,"",$5); print $5}')

[[ $fail -eq 0 ]] || { echo -e "${RED}unresolvable DT_NEEDED — do not stage this binary${NC}"; exit 1; }
echo -e "${GRN}all DT_NEEDED resolvable${NC}"
