#!/bin/sh
# Records what one device does, for the test hardware table.
#
# The script works with any device that gives an MTP interface. The script does
# not look for one make of cellphone.
#
# The script hides each file name. A capture goes into a repository, and a file
# name is private. See BSDROID_REDACT in the source.
#
# Usage:
#   sh tools/capture-device.sh <name>
#
# Example:
#   sh tools/capture-device.sh pixel-7a
#
# Before you run the script:
#   1. Connect the device.
#   2. Unlock the device.
#   3. Choose File transfer on the device.

set -eu

repo_root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
probe="$repo_root/target/debug/mtpprobe"

if [ $# -lt 1 ]; then
    echo "usage: sh tools/capture-device.sh <name>" >&2
    exit 2
fi
name=$1

if [ ! -x "$probe" ]; then
    echo "capture-device: cannot find $probe" >&2
    echo "  Build the project first: cargo build" >&2
    exit 2
fi

# Hide each file name in every command below.
BSDROID_REDACT=1
export BSDROID_REDACT

# Find the device. The probe reports the bus and the address, and the two
# numbers give the name of the device node.
echo "capture-device: look for a device with an MTP interface"
info=$("$probe" probe 2>&1 || true)
line=$(printf '%s\n' "$info" | grep -E '^  bus [0-9]+, address [0-9]+' | head -1 || true)

if [ -z "$line" ]; then
    echo "capture-device: no device gives an MTP interface." >&2
    echo "  Connect the device, unlock the device, and choose File transfer." >&2
    exit 1
fi

bus=$(printf '%s\n' "$line" | sed -E 's/.*bus ([0-9]+),.*/\1/')
addr=$(printf '%s\n' "$line" | sed -E 's/.*address ([0-9]+).*/\1/')
dev="ugen${bus}.${addr}"
echo "capture-device: the device is $dev"

out_dir="$repo_root/docs/captures"
mkdir -p "$out_dir"
out="$out_dir/$name.txt"

{
    echo "# Device capture: $name"
    echo "# node: $dev"
    echo

    echo "## usbconfig"
    usbconfig -d "$dev" dump_info 2>&1 || echo "(failed)"
    echo

    echo "## device descriptor"
    usbconfig -d "$dev" do_request 0x80 0x06 0x0100 0 18 2>&1 || echo "(failed)"
    echo

    echo "## BOS descriptor, which says what speeds the device supports"
    usbconfig -d "$dev" do_request 0x80 0x06 0x0F00 0 64 2>&1 ||
        echo "(none, so the device runs at high speed at most)"
    echo

    echo "## configuration descriptor"
    usbconfig -d "$dev" do_request 0x80 0x06 0x0200 0 256 2>&1 || echo "(failed)"
    echo

    echo "## interface classes"
    usbconfig -d "$dev" dump_curr_config_desc 2>&1 |
        grep -E 'bNumInterfaces|bInterfaceClass|bInterfaceSubClass|bInterfaceProtocol' ||
        echo "(failed)"
    echo

    echo "## sys.usb.config, if adb answers"
    adb shell getprop sys.usb.config 2>/dev/null | tail -1 || echo "(no adb)"
    echo

    echo "## mtpprobe caps"
    "$probe" caps 2>&1 || true
    echo

    echo "## mtpprobe probe"
    "$probe" probe 2>&1 || true
    echo

    echo "## mtpprobe objects"
    "$probe" objects 2>&1 || true
    echo

    echo "## mtpprobe reopen 10"
    "$probe" reopen 10 2>&1 || true
    echo

    echo "## mtpprobe bench"
    "$probe" bench 2>&1 || true
} > "$out" 2>&1

echo "capture-device: wrote $out"
echo
echo "Summary:"
grep -E '^RESULT|speed:|objects|storage\(s\)|MiB/s' "$out" | head -20
