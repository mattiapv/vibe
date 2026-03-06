#!/bin/bash
set -euo pipefail

EXCLUDE_FILE="${1:-.aiexclude}"
EMPTY_FILE="/tmp/aiexclude-empty-file"

if [ ! -f "$EXCLUDE_FILE" ]; then
    exit 0
fi

EXCLUDE_DIR="$(cd "$(dirname "$EXCLUDE_FILE")" && pwd -P)"

# One shared empty file can be bind-mounted over multiple file targets.
touch "$EMPTY_FILE"

is_mountpoint() {
    findmnt -n --mountpoint "$1" >/dev/null 2>&1
}

trim_whitespace() {
    local value="$1"
    value="${value#"${value%%[![:space:]]*}"}"
    value="${value%"${value##*[![:space:]]}"}"
    printf '%s' "$value"
}

mask_target() {
    local target="$1"

    if [ ! -e "$target" ]; then
        echo "Skipping '$target': path does not exist." >&2
        return
    fi

    if is_mountpoint "$target"; then
        return
    fi

    if [ -d "$target" ]; then
        mount -t tmpfs tmpfs "$target"
        return
    fi

    if [ -f "$target" ]; then
        mount --bind "$EMPTY_FILE" "$target"
        return
    fi

    echo "Skipping '$target': unsupported path type." >&2
}

has_glob_chars() {
    local value="$1"
    [[ "$value" == *"*"* || "$value" == *"?"* || "$value" == *"["* ]]
}

# Skip expensive/common dependency and build directories during recursive matching.
SKIP_RECURSIVE_DIRS=(
    .git
    node_modules
    target
    __pycache__
    .venv
    venv
    env
    .tox
    .nox
    .pytest_cache
    .mypy_cache
    .ruff_cache
    .cache
    dist
    build
    .next
    .nuxt
    .svelte-kit
)

find_recursive_matches() {
    local pattern="$1"
    find "$EXCLUDE_DIR" \
        \( -type d \( \
            -name "${SKIP_RECURSIVE_DIRS[0]}" \
            -o -name "${SKIP_RECURSIVE_DIRS[1]}" \
            -o -name "${SKIP_RECURSIVE_DIRS[2]}" \
            -o -name "${SKIP_RECURSIVE_DIRS[3]}" \
            -o -name "${SKIP_RECURSIVE_DIRS[4]}" \
            -o -name "${SKIP_RECURSIVE_DIRS[5]}" \
            -o -name "${SKIP_RECURSIVE_DIRS[6]}" \
            -o -name "${SKIP_RECURSIVE_DIRS[7]}" \
            -o -name "${SKIP_RECURSIVE_DIRS[8]}" \
            -o -name "${SKIP_RECURSIVE_DIRS[9]}" \
            -o -name "${SKIP_RECURSIVE_DIRS[10]}" \
            -o -name "${SKIP_RECURSIVE_DIRS[11]}" \
            -o -name "${SKIP_RECURSIVE_DIRS[12]}" \
            -o -name "${SKIP_RECURSIVE_DIRS[13]}" \
            -o -name "${SKIP_RECURSIVE_DIRS[14]}" \
            -o -name "${SKIP_RECURSIVE_DIRS[15]}" \
            -o -name "${SKIP_RECURSIVE_DIRS[16]}" \
            -o -name "${SKIP_RECURSIVE_DIRS[17]}" \
        \) -prune \) -o \
        -name "$pattern" -print0 2>/dev/null
}

while IFS= read -r raw_line || [ -n "$raw_line" ]; do
    line="$(trim_whitespace "$raw_line")"

    if [ -z "$line" ] || [ "${line:0:1}" = "#" ]; then
        continue
    fi

    # Match bare filenames recursively (gitignore-like behavior).
    if [[ "$line" != */* ]] && ! has_glob_chars "$line"; then
        matched=0
        while IFS= read -r -d '' target; do
            matched=1
            mask_target "$target"
        done < <(find_recursive_matches "$line")
        if [ "$matched" -eq 0 ]; then
            echo "Skipping '$line': no recursive matches found." >&2
        fi
        continue
    fi

    if [[ "$line" = /* ]]; then
        resolved="$line"
    else
        resolved="$EXCLUDE_DIR/$line"
    fi

    if has_glob_chars "$line" && [[ "$line" != */* ]]; then
        matched=0
        while IFS= read -r -d '' target; do
            matched=1
            mask_target "$target"
        done < <(find_recursive_matches "$line")
        if [ "$matched" -eq 0 ]; then
            echo "Skipping '$line': no recursive matches for pattern." >&2
        fi
        continue
    fi

    if has_glob_chars "$resolved"; then
        matched=0
        while IFS= read -r target; do
            matched=1
            mask_target "$target"
        done < <(compgen -G "$resolved" || true)
        if [ "$matched" -eq 0 ]; then
            echo "Skipping '$line': no matches for pattern." >&2
        fi
        continue
    fi

    mask_target "$resolved"
done < "$EXCLUDE_FILE"
