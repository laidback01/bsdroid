#!/bin/sh
# Pull the cable while the host reads or writes. Does the code stop?
#
# This project exists because an MTP stack on FreeBSD entered an endless loop.
# See docs/00-why.md. Two rules come from that defect:
#
#   1. No loop waits on the device for its end condition.
#   2. Every transfer has a deadline.
#
# A unit test cannot check either rule, because a test cannot make hardware
# leave the bus. A person with a cable can.
#
# The check runs until the time is up. A person pulls the cable at any moment,
# as often as they like, and puts it back. The harness rides out the whole
# cycle: it mounts, works, meets the fault, unmounts, waits, and mounts again.
#
#   A read or a write that fails is a pass. The device is gone.
#   A read or a write that never returns is the fault this project avoids.
#   A write that reports success and holds wrong bytes is worse than a fault.
#   A write that reports success after the mount reported a fault is the
#   worst of the four. The program was told a lie. See
#   docs/07-filesystem-design.md.
#
# Usage:
#   tools/chaos-cable.sh <mount point> <vendor:product> [read|write] [seconds]
#
# Example:
#   tools/chaos-cable.sh /mnt/phone 04e8:6860 write 600
#
# Run `mtpfs -l` for the vendor and product of a cellphone. The check looks the
# node up each time, because a cellphone can come back on a different node.
#
# The write mode makes one folder, `bsdroid-chaos`, and removes it at the end.
# Nothing outside that folder is touched.
#
# Exit status:
#   0   Every command came back, and every write that finished was correct.
#   1   A command hung, or a write gave wrong bytes, or a write was lost.
#   2   The check could not run.

set -u

MNT=${1:-}
VIDPID=${2:-}
MODE=${3:-read}
RUN_FOR=${4:-600}
MB=24

if [ -z "$MNT" ] || [ -z "$VIDPID" ]; then
    echo "usage: $0 <mount point> <vendor:product> [read|write] [seconds]" >&2
    exit 2
fi

repo_root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
FS=$repo_root/target/release/mtpfs
[ -x "$FS" ] || FS=$repo_root/target/debug/mtpfs
if [ ! -x "$FS" ]; then
    echo "chaos-cable: build mtpfs first, with cargo build --release" >&2
    exit 2
fi

SCRATCH=$MNT/bsdroid-chaos
LOG=${TMPDIR:-/var/tmp}/chaos-cable-$$.log
SRC=${TMPDIR:-/var/tmp}/chaos-cable-src-$$.bin
: >"$LOG"

note() { echo "$(date '+%H:%M:%S') $*"; }

cleanup_mount() {
    timeout 30 rm -rf "$SCRATCH" 2>/dev/null || true
    timeout 20 umount "$MNT" 2>/dev/null || timeout 20 umount -f "$MNT" 2>/dev/null || true
}
# Keep the log. It holds what the mount said about each device, and a
# run that finds something is worth reading afterwards. An earlier
# version removed it, and the detail of a whole test went with it.
trap 'cleanup_mount; rm -f "$SRC"' EXIT

resolve_node() {
    "$FS" -l 2>/dev/null | awk -v vp="$VIDPID" '$2 == vp {print $1; exit}'
}

if [ "$MODE" = write ]; then
    note "building a source file of $MB MiB"
    dd if=/dev/urandom of="$SRC" bs=1m count="$MB" 2>/dev/null
    SUM=$(sha256 -q "$SRC")
fi

ok=0; failed=0; hung=0; corrupt=0; lost=0; cycles=0; slowest=0
shown=""

mkdir -p "$MNT"
cleanup_mount

note "watching $VIDPID for $RUN_FOR seconds, in $MODE mode."
note "the mount writes to $LOG"
note "Pull the cable whenever you like. A fault is a pass."

START=$(date +%s)
while [ $(( $(date +%s) - START )) -lt "$RUN_FOR" ]; do
    NODE=$(resolve_node)
    if [ -z "$NODE" ]; then
        [ "$shown" != gone ] && { note "the cellphone is not there"; shown=gone; }
        sleep 3
        continue
    fi
    if [ "$shown" != "$NODE" ]; then
        note "the cellphone is at $NODE"
        shown=$NODE
    fi

    if ! mount | grep -q " $MNT "; then
        ( "$FS" -f "$NODE" "$MNT" >>"$LOG" 2>&1 & ) </dev/null
        i=0
        while [ "$i" -lt 12 ]; do
            mount | grep -q " $MNT " && break
            sleep 1
            i=$((i + 1))
        done
        mount | grep -q " $MNT " || { sleep 4; continue; }
        cycles=$((cycles + 1))
        note "mount $cycles is up"
    fi

    if [ "$MODE" = write ]; then
        timeout 60 mkdir -p "$SCRATCH" 2>/dev/null || { cleanup_mount; sleep 3; continue; }
        TARGETS=$SCRATCH/chaos.bin
    else
        # The read ahead cache holds 4 MiB for one object. A small file read
        # again and again never reaches the bus after the first pass, so the
        # check would measure memory. Take the large files, and alternate.
        TARGETS=$(timeout 90 find "$MNT" -maxdepth 3 -type f -size +4M 2>/dev/null | head -8)
        [ -z "$TARGETS" ] && { note "no file above 4 MiB to read"; cleanup_mount; sleep 3; continue; }
        COUNT_T=$(printf '%s\n' "$TARGETS" | wc -l | tr -d ' ')
    fi

    run_failures=0
    while [ $(( $(date +%s) - START )) -lt "$RUN_FOR" ]; do
        before=$(grep -c "cannot write" "$LOG" 2>/dev/null); before=${before:-0}
        t0=$(date +%s)

        if [ "$MODE" = write ]; then
            if timeout -k 5 120 cp "$SRC" "$TARGETS" >/dev/null 2>&1; then rc=0; else rc=$?; fi
        else
            T=$(printf '%s\n' "$TARGETS" | sed -n "$(( (ok + failed + hung) % COUNT_T + 1 ))p")
            if timeout -k 5 45 cat "$T" >/dev/null 2>&1; then rc=0; else rc=$?; fi
        fi

        after=$(grep -c "cannot write" "$LOG" 2>/dev/null); after=${after:-0}
        took=$(( $(date +%s) - t0 ))
        [ "$took" -gt "$slowest" ] && slowest=$took

        case "$rc" in
            0)
                if [ "$after" -gt "$before" ]; then
                    lost=$((lost + 1))
                    note "SILENT LOSS: the mount reported a fault and the program saw none"
                fi
                if [ "$MODE" = write ]; then
                    GOT=$(timeout -k 5 120 sha256 -q "$TARGETS" 2>/dev/null || echo unreadable)
                    if [ "$GOT" = "$SUM" ]; then
                        ok=$((ok + 1)); run_failures=0
                    elif [ "$GOT" = unreadable ]; then
                        failed=$((failed + 1)); run_failures=$((run_failures + 1))
                    else
                        corrupt=$((corrupt + 1)); run_failures=0
                        note "WRONG BYTES: a write reported success and the sums differ"
                    fi
                    timeout 30 rm -f "$TARGETS" 2>/dev/null || true
                else
                    ok=$((ok + 1)); run_failures=0
                fi
                ;;
            124|137)
                hung=$((hung + 1)); run_failures=$((run_failures + 1))
                note "HUNG: a command did not return inside its deadline (rc=$rc)"
                ;;
            *)
                failed=$((failed + 1)); run_failures=$((run_failures + 1))
                [ "$run_failures" -eq 1 ] && note "a command failed after ${took}s. This is a pass."
                ;;
        esac

        if [ "$run_failures" -ge 4 ]; then
            note "mount $cycles is finished. Waiting for the cellphone."
            cleanup_mount
            break
        fi
    done
done

cleanup_mount

echo
note "---- finished ----"
note "mounts that came up:     $cycles"
note "commands that worked:    $ok"
note "commands that failed:    $failed   (a fault is a pass)"
note "commands that hung:      $hung     (this is the fault we look for)"
if [ "$MODE" = write ]; then
    note "writes with wrong bytes: $corrupt"
    note "writes lost in silence:  $lost"
fi
note "slowest command:         ${slowest}s"

if [ "$hung" -gt 0 ] || [ "$corrupt" -gt 0 ] || [ "$lost" -gt 0 ]; then
    note "RESULT: a command hung, or a write gave wrong bytes, or a write was lost."
    exit 1
fi
note "the mount log is at $LOG"
note "RESULT: every command came back. A timeout is a result, and not a hang."
