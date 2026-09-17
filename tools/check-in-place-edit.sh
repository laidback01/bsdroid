#!/bin/sh
# Checks a change to a file that is on the cellphone.
#
# The script checks the contents after each step, and not the exit status
# alone. The code before the in-place change passed 4 of these 19 checks.
# Usage: editprobe2.sh <mtpfs> <vendor:product> <label> <outdir>
set -u
FS=$1; IDS=$2; LABEL=$3; OUT=$4
MNT=$OUT/edit-$LABEL; S=$MNT/bsdroid-edit
pass=0; fail=0
mkdir -p "$MNT" "$OUT"; umount "$MNT" 2>/dev/null
( "$FS" -f "$IDS" "$MNT" >"$OUT/$LABEL-edit.log" 2>&1 & ) </dev/null
i=0; while [ "$i" -lt 25 ]; do mount | grep -q " $MNT " && break; sleep 1; i=$((i+1)); done
mount | grep -q " $MNT " || { echo "  MOUNT FAILED"; sed 's/^/    /' "$OUT/$LABEL-edit.log"; exit 1; }
echo "=== $LABEL ==="
timeout 60 rm -rf "$S" 2>/dev/null; timeout 60 mkdir "$S" || { echo "  no scratch"; exit 1; }

chk() { # chk <label> <file> <expected>
  got=$(timeout 90 cat "$2" 2>/dev/null)
  if [ "$got" = "$3" ]; then printf '  %-40s ok\n' "$1"; pass=$((pass+1))
  else printf '  %-40s WRONG: [%s] wanted [%s]\n' "$1" "$got" "$3"; fail=$((fail+1)); fi
}
one() { # one <label> <shell command>
  err=$(timeout 90 sh -c "$2" 2>&1); rc=$?
  if [ "$rc" = 0 ]; then printf '  %-40s ok\n' "$1"; pass=$((pass+1))
  else printf '  %-40s rc=%s %s\n' "$1" "$rc" "${err##*: }"; fail=$((fail+1)); fi
}

one "write a new file"        "printf 'one\n' > '$S/a.txt'"
chk "  and it holds"          "$S/a.txt" "one"
one "copy onto an existing file" "printf 'two two\n' > '$OUT/s2'; cp '$OUT/s2' '$S/a.txt'"
chk "  and it holds"          "$S/a.txt" "two two"
one "truncate with a shell"   ": > '$S/a.txt'"
chk "  and it is empty"       "$S/a.txt" ""
one "append to it"            "printf 'three\n' >> '$S/a.txt'"
chk "  and it holds"          "$S/a.txt" "three"
one "append again"            "printf 'four\n' >> '$S/a.txt'"
chk "  and it holds both"     "$S/a.txt" "$(printf 'three\nfour')"
one "edit in place with sed -i" "sed -i '' 's/four/FOUR/' '$S/a.txt'"
chk "  and it holds the edit" "$S/a.txt" "$(printf 'three\nFOUR')"
one "truncate -s 0 with no open" "truncate -s 0 '$S/a.txt'"
chk "  and it is empty"       "$S/a.txt" ""
one "write a temp beside it"  "printf 'final\n' > '$S/a.txt.tmp'"
one "rename the temp over it" "mv '$S/a.txt.tmp' '$S/a.txt'"
chk "  and it holds"          "$S/a.txt" "final"
one "rename to a free name"   "mv '$S/a.txt' '$S/b.txt'"
chk "  and it holds"          "$S/b.txt" "final"
left=$(timeout 60 ls "$S" 2>/dev/null | tr '\n' ' ')
printf '  %-40s %s\n' "the folder now holds" "$left"
case "$left" in *bsdroid-old*|*".tmp"*) echo "  LEFTOVER OBJECTS"; fail=$((fail+1));; esac
[ "$left" = "b.txt " ] || { echo "  UNEXPECTED CONTENTS"; fail=$((fail+1)); }

echo "  mount reported:"; grep -E "cannot|Read-only" "$OUT/$LABEL-edit.log" | sed 's/^/    /' | head -8
printf '  RESULT %s pass %s fail\n' "$pass" "$fail"
timeout 120 rm -rf "$S" 2>/dev/null
umount "$MNT" 2>/dev/null || umount -f "$MNT" 2>/dev/null
