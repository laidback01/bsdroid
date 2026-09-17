#!/bin/sh
# Checks that a failed write reaches the program that wrote it.
#
# mtpfs holds a new file in a spool file on the host, and sends the whole
# object to the cellphone when the program closes the file. The send can fail.
# This check answers one question:
#
#   Does the program find out?
#
# An earlier version of mtpfs sent the object from the FUSE callback `release`.
# The kernel throws away the answer of `release`, so every fault went nowhere.
# A copy that never reached the cellphone reported success. See
# docs/07-filesystem-design.md.
#
# The check needs a write that fails, needs no cable in anybody's hand, and
# moves no bytes. A file above the 4 GiB limit of MTP is one: the host refuses
# it before it sends anything, because the length field of a container cannot
# count that many bytes.
#
# The steps are open, ftruncate and close. Only close can carry the answer.
#
# Usage:
#   tools/check-write-errors.sh <mount point> [node]
#
# Example:
#   tools/check-write-errors.sh /mnt/phone ugen0.11
#
# Exit status:
#   0   The fault reached the program.
#   1   The fault did not reach the program. A write can go missing in silence.
#   2   The check could not run.

set -eu

MNT=${1:-}
NODE=${2:-}

if [ -z "$MNT" ]; then
    echo "usage: $0 <mount point> [node]" >&2
    exit 2
fi

repo_root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
FS=$repo_root/target/release/mtpfs
if [ ! -x "$FS" ]; then
    FS=$repo_root/target/debug/mtpfs
fi
if [ ! -x "$FS" ]; then
    echo "check-write-errors: build mtpfs first, with cargo build --release" >&2
    exit 2
fi

if ! command -v python3 >/dev/null 2>&1; then
    echo "check-write-errors: this check needs python3" >&2
    exit 2
fi

SCRATCH=$MNT/bsdroid-write-check
mounted_here=no

cleanup() {
    rm -rf "$SCRATCH" 2>/dev/null || true
    if [ "$mounted_here" = yes ]; then
        umount "$MNT" 2>/dev/null || umount -f "$MNT" 2>/dev/null || true
    fi
}
trap cleanup EXIT

# Mount, unless the caller already did.
if ! mount | grep -q " $MNT "; then
    mkdir -p "$MNT"
    # mtpfs keeps the stdout it inherited, so a shell that waits for it
    # blocks. Start it detached and watch the mount table.
    ( "$FS" ${NODE:+"$NODE"} "$MNT" >/dev/null 2>&1 & ) </dev/null
    i=0
    while [ "$i" -lt 25 ]; do
        mount | grep -q " $MNT " && break
        sleep 1
        i=$((i + 1))
    done
    if ! mount | grep -q " $MNT "; then
        echo "check-write-errors: cannot mount $MNT" >&2
        exit 2
    fi
    mounted_here=yes
fi

mkdir -p "$SCRATCH"

python3 - "$SCRATCH/too-big.bin" <<'PY'
import os
import sys

# Above MAX_PAYLOAD_LEN, which is just under 4 GiB. The host refuses this
# size before it sends one byte.
TOO_BIG = 5 * 1024**3
path = sys.argv[1]

fd = os.open(path, os.O_CREAT | os.O_WRONLY | os.O_TRUNC, 0o644)
try:
    os.ftruncate(fd, TOO_BIG)
except OSError as e:
    print(f"check-write-errors: ftruncate failed: {e}")
    os.close(fd)
    sys.exit(2)

try:
    os.close(fd)
except OSError as e:
    print(f"ok    close() reported the fault: {e.strerror}, errno {e.errno}")
    sys.exit(0)

print("FAIL  close() succeeded, and the write never reached the cellphone.")
print("      A program that copies a file cannot see this fault.")
print("      The send belongs in flush. See docs/07-filesystem-design.md.")
sys.exit(1)
PY
