#!/bin/sh
# Records what a device gives in one USB mode.
#
# The script writes one file for each mode. A test then uses the file, and a
# reader compares two modes.
#
# Usage:
#   sh tools/capture-mode.sh <name> [ugen-device]
#
# Example:
#   sh tools/capture-mode.sh file-transfer
#   sh tools/capture-mode.sh midi ugen0.11
#
# The script needs a connected device. The script does not change the device.

set -eu

repo_root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)

if [ $# -lt 1 ]; then
    echo "usage: sh tools/capture-mode.sh <name> [ugen-device]" >&2
    exit 2
fi

name=$1
dev=${2:-}

# Find the device, if the caller gave none.
if [ -z "$dev" ]; then
    dev=$(usbconfig list 2>/dev/null | grep -i samsung | head -1 | cut -d: -f1)
fi
if [ -z "$dev" ]; then
    echo "capture-mode: cannot find the device. Give the name, such as ugen0.11." >&2
    exit 2
fi

out_dir="$repo_root/docs/captures"
mkdir -p "$out_dir"
out="$out_dir/$name.txt"

{
    echo "# USB mode capture: $name"
    echo "# device: $dev"
    echo

    echo "## usbconfig list"
    usbconfig list 2>&1 | grep -i samsung || echo "(no Samsung device)"
    echo

    echo "## device descriptor"
    usbconfig -d "$dev" do_request 0x80 0x06 0x0100 0 18 2>&1 || echo "(failed)"
    echo

    echo "## configuration descriptor"
    usbconfig -d "$dev" do_request 0x80 0x06 0x0200 0 256 2>&1 || echo "(failed)"
    echo

    echo "## interface classes"
    usbconfig -d "$dev" dump_curr_config_desc 2>&1 |
        grep -E 'bInterfaceClass|bInterfaceSubClass|bInterfaceProtocol|bNumInterfaces' ||
        echo "(failed)"
    echo

    echo "## sys.usb.config, if adb answers"
    adb shell getprop sys.usb.config 2>&1 | tail -1 || echo "(no adb)"
    echo

    echo "## screen lock state, if adb answers"
    adb shell dumpsys window 2>/dev/null | grep -i mDreamingLockscreen | head -1 ||
        echo "(no adb)"
    echo

    echo "## mtpprobe"
    "$repo_root/target/debug/mtpprobe" 2>&1 || true
} > "$out" 2>&1

echo "capture-mode: wrote $out"
grep -E '^RESULT|bInterfaceClass' "$out" | head -8
