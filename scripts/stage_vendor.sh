#!/bin/bash
# FaceHAL/scripts/stage_vendor.sh
#
# Installs the AIDL face HAL into the unpacked vendor tree of the oos project so
# the next repack ships it. This ONLY writes files into erofs/vendor — it never
# repacks an image and never touches the device.
#
# Naming is dictated by the vendor's own sepolicy, which already contains
#   /(vendor|system/vendor)/bin/hw/android\.hardware\.biometrics\.face-service\.example
#       u:object_r:hal_face_default_exec:s0
# (erofs/vendor/etc/selinux/vendor_file_contexts). Shipping the binary under that
# exact name means it inherits hal_face_default_exec with no sepolicy change.
set -euo pipefail

HERE="$(dirname "$(dirname "$(readlink -f "${BASH_SOURCE[0]}")")")"
ROM_TREE="${ROM_TREE:-$HERE/../../erofs}"
VENDOR="$ROM_TREE/vendor"

RED=$'\033[1;31m'; GRN=$'\033[1;32m'; YEL=$'\033[1;33m'; NC=$'\033[0m'

SRC="$HERE/target/aarch64-linux-android/release/face-hal-service"
[[ -f "$SRC" ]] || { echo -e "${RED}no built binary — run scripts/build.sh first${NC}"; exit 1; }

# The unpacked ROM tree is a sibling checkout, not part of this repository.
[[ -d "$VENDOR" ]] || {
    echo -e "${RED}no vendor tree at $VENDOR${NC}"
    echo "  set ROM_TREE to the unpacked tree this HAL is staged into"
    exit 1
}

HAL_NAME="android.hardware.biometrics.face-service.example"
RC_NAME="android.hardware.biometrics.face-service.rc"
FQNAME="android.hardware.biometrics.face.IFace/default"

echo -e "${GRN}staging into${NC} $VENDOR"

# --- 1. binary -------------------------------------------------------------
install -D -m 0755 "$SRC" "$VENDOR/bin/hw/$HAL_NAME"
echo "  bin/hw/$HAL_NAME ($(stat -c%s "$VENDOR/bin/hw/$HAL_NAME") bytes)"

# --- 2. init rc ------------------------------------------------------------
# Mirrors the stock HIDL face rc's deliberate `class late_start`: that rc
# documents a race on this device where `class hal` starts the HAL before /data
# is mounted. `interface aidl` keeps it lazily registered.
cat > "$VENDOR/etc/init/$RC_NAME" <<EOF
service vendor.face-hal-aidl-default /vendor/bin/hw/$HAL_NAME
    interface aidl android.hardware.biometrics.face.IFace/default
    # class hal races with /data on this device; the stock HIDL face rc uses
    # late_start for the same reason.
    class late_start
    user system
    group system
    task_profiles ProcessCapacityHigh
    capabilities SYS_NICE
    rlimit rtprio 10 10
EOF
echo "  etc/init/$RC_NAME"

# --- 3. VINTF manifest -----------------------------------------------------
# The effective device manifest is /vendor/etc/vintf/manifest.xml; the fragment
# files under etc/vintf/manifest/ are not referenced by it, so the AIDL HAL has
# to be declared inline.
MANIFEST="$VENDOR/etc/vintf/manifest.xml"
if grep -q 'format="aidl"' "$MANIFEST" && grep -A3 'format="aidl"' "$MANIFEST" | grep -q "android.hardware.biometrics.face"; then
    echo "  etc/vintf/manifest.xml (AIDL face entry already present)"
else
    python3 - "$MANIFEST" <<'PY'
import sys, re
p = sys.argv[1]
s = open(p).read()
entry = (
    '    <hal format="aidl">\n'
    '        <name>android.hardware.biometrics.face</name>\n'
    '        <version>3</version>\n'
    '        <interface>\n'
    '            <name>IFace</name>\n'
    '            <instance>default</instance>\n'
    '        </interface>\n'
    '        <fqname>IFace/default</fqname>\n'
    '    </hal>\n'
)
# match the indentation of the surrounding entries
m = re.search(r'\n([ \t]*)</manifest>', s)
if not m:
    sys.exit("no closing </manifest> tag found")
indent = m.group(1)
entry = entry.replace('\n    ', '\n' + indent)
s = s[:m.start()] + '\n' + indent + entry.rstrip('\n') + '\n' + s[m.start()+1:]
open(p, 'w').write(s)
print("  etc/vintf/manifest.xml (AIDL face entry added)")
PY
fi

# --- 4. verify the wiring the way a staging script must --------------------
fail=0

# rc service path must match the staged filename ($3: service <name> <path>)
rc_path=$(awk '/^service /{print $3}' "$VENDOR/etc/init/$RC_NAME")
[[ "$rc_path" == "/vendor/bin/hw/$HAL_NAME" ]] \
    || { echo -e "  ${RED}FAIL rc path $rc_path != /vendor/bin/hw/$HAL_NAME${NC}"; fail=1; }

# rc-declared interface must equal the name the binary registers ($3: interface aidl <fqname>)
rc_iface=$(awk '/^    interface /{print $3}' "$VENDOR/etc/init/$RC_NAME")
[[ "$rc_iface" == "$FQNAME" ]] \
    || { echo -e "  ${RED}FAIL rc interface $rc_iface != $FQNAME${NC}"; fail=1; }

# sepolicy: the repack's gen_file-contexts is a gap-filler. It PRESERVES entries
# already in config/<partition>_file_contexts and only assigns a blanket
# u:object_r:hal_allocator_default_exec:s0 to files with no entry (verified by
# running it against a scratch copy). So a brand-new /vendor/bin/hw binary needs
# an existing entry or it comes out with the wrong domain.
FC="$ROM_TREE/config/vendor_file_contexts"
# Compare against the backslash-escaped form gen_file-contexts writes.
FC_STRIPPED="$(tr -d '\\' < "$FC")"
if printf '%s\n' "$FC_STRIPPED" | grep -qF "/vendor/bin/hw/$HAL_NAME "; then
    ctx=$(printf '%s\n' "$FC_STRIPPED" | grep -F "/vendor/bin/hw/$HAL_NAME " | awk '{print $2}')
    echo "  sepolicy context : $ctx (already in config/vendor_file_contexts)"
else
    ctx="u:object_r:hal_allocator_default_exec:s0 (WOULD BE AUTO-GENERATED — WRONG DOMAIN)"
    echo -e "  ${YEL}WARN no entry for /vendor/bin/hw/$HAL_NAME in config/vendor_file_contexts${NC}"
    echo -e "  ${YEL}WARN the repack will label it hal_allocator_default_exec, not hal_face_default_exec${NC}"
    echo -e "  ${YEL}WARN the service would then run in the allocator domain and be denied${NC}"
fi

# manifest must declare the AIDL HAL at the version we implement
grep -A2 "<name>android.hardware.biometrics.face</name>" "$MANIFEST" | grep -q "<version>3</version>" \
    || { echo -e "  ${RED}FAIL manifest does not declare face AIDL v3${NC}"; fail=1; }

[[ $fail -eq 0 ]] || { echo -e "${RED}staging verification failed${NC}"; exit 1; }

echo
echo -e "${GRN}staged and verified${NC}"
[[ "$ctx" == *WRONG* ]] && echo -e "  ${YEL}ACTION NEEDED: see note below${NC}" || echo "  sepolicy context : $ctx"
echo "  registers        : $FQNAME"
echo "  manifest         : android.hardware.biometrics.face AIDL v3"
echo
echo "Nothing was repacked or flashed. Repack vendor when you are ready:"
echo "  cd /mnt/project/RomTools && source tool/functions && \\"
echo "    export erofsdir=/mnt/project/RomTools/ROM_WORKDIR/oos/erofs && \\"
echo "    export DIROUT=/mnt/project/RomTools/output && \\"
echo "    export usable_threads=\$(nproc) && repack_erofs_cmd vendor lz4hc 0"
