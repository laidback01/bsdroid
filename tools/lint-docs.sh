#!/bin/sh
# Checks every Markdown file against Simplified Technical English rules.
#
# The check uses ste_lint.py from the SimpleEnglish project:
#   https://github.com/AminBlg/SimpleEnglish   (MIT licence)
#
# This project does not copy the linter. The script finds the linter, or the
# script tells you how to get the linter.
#
# Usage:
#   tools/lint-docs.sh
#   STE_LINT=/path/to/ste_lint.py tools/lint-docs.sh
#
# Exit status:
#   0   Every file passes.
#   1   A file holds a violation.
#   2   The script cannot find the linter or Python.

set -eu

repo_root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)

# --- Find Python ---
if command -v python3 >/dev/null 2>&1; then
    python=python3
elif command -v python >/dev/null 2>&1; then
    python=python
else
    echo "lint-docs: no Python interpreter." >&2
    echo "  FreeBSD:  doas pkg install python3" >&2
    exit 2
fi

# --- Find the linter ---
# `npx skills add` does not give you the linter. The command writes SKILL.md
# and the reference files only. You must clone the repository for the linter.
#
# The search order:
#   1. The STE_LINT environment variable.
#   2. A clone beside this repository.
#   3. A clone in the home directory.
if [ -n "${STE_LINT:-}" ]; then
    lint=$STE_LINT
else
    lint=""
    for candidate in \
        "$repo_root/../SimpleEnglish/evals/ste_lint.py" \
        "$HOME/SimpleEnglish/evals/ste_lint.py" \
        "$HOME/src/SimpleEnglish/evals/ste_lint.py"
    do
        if [ -f "$candidate" ]; then
            lint=$candidate
            break
        fi
    done
fi

if [ -z "$lint" ] || [ ! -f "$lint" ]; then
    echo "lint-docs: cannot find ste_lint.py." >&2
    echo "  Clone the project beside this repository:" >&2
    echo "    git clone --depth 1 https://github.com/AminBlg/SimpleEnglish.git" >&2
    echo "  Or set STE_LINT to the path of the file." >&2
    exit 2
fi

# --- Check each file ---
status=0
count=0

# `find` gives a stable order, and the order makes the output easy to compare.
files=$(find "$repo_root" -name '*.md' -not -path '*/target/*' -not -path '*/.git/*' | sort)

for file in $files; do
    count=$((count + 1))
    report=$("$python" "$lint" "$file" 2>&1)

    # The linter answers with JSON. Read the total with a short Python program,
    # because the base system has no JSON tool.
    total=$(printf '%s' "$report" | "$python" -c \
        'import json,sys; print(json.load(sys.stdin).get("violations_total", "?"))' \
        2>/dev/null || echo "?")

    name=${file#"$repo_root"/}

    if [ "$total" = "0" ]; then
        echo "ok    $name"
    else
        echo "FAIL  $name   violations: $total"
        printf '%s\n' "$report" | sed 's/^/      /'
        status=1
    fi
done

echo ""
if [ "$status" -eq 0 ]; then
    echo "lint-docs: $count files, 0 violations."
else
    echo "lint-docs: a file holds a violation. Read the report above." >&2
fi

exit "$status"
