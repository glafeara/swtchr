#!/usr/bin/env bash
# Fetch + assemble the Russian word list for swtchr.
#
# Pipeline:
#   1. download ru_full.txt from FrequencyWords (~32 MB, ~1.3M entries,
#      OpenSubtitles-2018, frequency-ordered word *forms* — already inflected,
#      naturally covers slang/anglicisms);
#   2. keep only Cyrillic-leading lines (drops Latin tokens & OCR garbage),
#      strip the count column, take the top 200k;
#   3. append the curated supplement (dist/ru_supplement.txt) which patches
#      post-2018 vocabulary (covid, AI, релокация) and IT-jargon inflections;
#   4. sort -u → ${DEST}/ru_top200k.txt.
#
# Source: https://github.com/hermitdave/FrequencyWords (CC-BY-SA-4.0)
#
# Usage:
#   dist/fetch-dicts.sh          # fetch if missing
#   dist/fetch-dicts.sh --force  # re-download even if present

set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
SUPPLEMENT="${SCRIPT_DIR}/ru_supplement.txt"

DEST_DIR="${XDG_DATA_HOME:-${HOME}/.local/share}/swtchr/dicts"
RU_URL="https://raw.githubusercontent.com/hermitdave/FrequencyWords/master/content/2018/ru/ru_full.txt"
RU_DST="${DEST_DIR}/ru_top200k.txt"
RU_LEGACY="${DEST_DIR}/ru_50k.txt"

TOP_N=200000
RAW_MIN_BYTES=10000000   # ru_full.txt is ~32 MB; flag obvious truncation
OUT_MIN_LINES=150000     # sanity-check the assembled output

force=0
[[ "${1:-}" == "--force" ]] && force=1

step() { printf '\033[1;34m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33m!!\033[0m  %s\n' "$*" >&2; }
fatal() {
    printf '\033[1;31mxx\033[0m  %s\n' "$*" >&2
    exit 1
}

command -v curl >/dev/null || fatal "curl not in PATH"
command -v grep >/dev/null || fatal "grep not in PATH"
command -v awk  >/dev/null || fatal "awk not in PATH"
command -v sort >/dev/null || fatal "sort not in PATH"

mkdir -p "${DEST_DIR}"

# Skip the whole pipeline if the assembled output already looks good.
if [[ -s "${RU_DST}" && ${force} -eq 0 ]]; then
    lines=$(wc -l <"${RU_DST}")
    if (( lines >= OUT_MIN_LINES )); then
        step "skip: ${RU_DST} already present (${lines} lines)"
        exit 0
    fi
    warn "${RU_DST} present but only ${lines} lines; rebuilding"
fi

tmp_raw="$(mktemp -t swtchr_ru_full.XXXXXX)"
tmp_out="$(mktemp -t swtchr_ru_top.XXXXXX)"
trap 'rm -f "${tmp_raw}" "${tmp_out}"' EXIT

step "downloading ${RU_URL}"
curl --fail --location --silent --show-error --output "${tmp_raw}" "${RU_URL}"
sz=$(stat -c%s "${tmp_raw}")
if (( sz < RAW_MIN_BYTES )); then
    fatal "downloaded only ${sz} bytes — bad URL or truncated transfer"
fi
step "downloaded ${sz} bytes"

step "filtering Cyrillic-leading lines, taking top ${TOP_N}"
# UTF-8 PCRE: first char must be in the Cyrillic block (U+0400..U+04FF).
# `grep -m N` stops after N matches — preferable to `| head` which would
# trigger SIGPIPE under `set -o pipefail`. Drop the count column with awk.
LC_ALL=C.UTF-8 grep -m "${TOP_N}" -P '^[\x{0400}-\x{04FF}]' "${tmp_raw}" \
    | awk '{print $1}' \
    > "${tmp_out}"

raw_lines=$(wc -l <"${tmp_out}")
step "kept ${raw_lines} freq-list entries"

if [[ -f "${SUPPLEMENT}" ]]; then
    step "merging supplement: ${SUPPLEMENT}"
    awk '
        /^[[:space:]]*#/ { next }
        /^[[:space:]]*$/ { next }
        { for (i = 1; i <= NF; i++) print $i }
    ' "${SUPPLEMENT}" >> "${tmp_out}"
else
    warn "supplement not found at ${SUPPLEMENT} — proceeding with freq list only"
fi

step "deduplicating"
LC_ALL=C sort -u "${tmp_out}" -o "${tmp_out}"
final_lines=$(wc -l <"${tmp_out}")

if (( final_lines < OUT_MIN_LINES )); then
    fatal "assembled list only ${final_lines} lines — refusing to install"
fi

mv "${tmp_out}" "${RU_DST}"
step "wrote ${RU_DST} (${final_lines} entries)"

if [[ -f "${RU_LEGACY}" ]]; then
    step "removing legacy ${RU_LEGACY}"
    rm -f "${RU_LEGACY}"
    warn "if your config.toml still points at ru_50k.txt, update it to ru_top200k.txt"
fi

step "done. config path: ${RU_DST}"
