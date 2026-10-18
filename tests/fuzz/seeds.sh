#!/usr/bin/env bash
# Seeds a fuzz corpus directory (nightly `fuzz` and `fuzz-corpus` jobs), from the repository root:
#
#   tests/fuzz/seeds.sh <target> <corpus dir>
#
# Seeds are this repository's own files. The interop plugin's harnesses (xschem_read,
# sfyx_import) and their seeds live in its repository (github.com/OmarSiwy/Schemify-Interop).
set -eu

target="$1"
dir="$2"
mkdir -p "$dir"
case "$target" in
  parse)
    find crates/*/tests/fixtures -name '*.sh*' -type f -exec cp {} "$dir"/ \;
    ;;
  merge3)
    # O, A, B: every pair (f, g) of format fixtures as (f, g, f), NUL-separated.
    for f in crates/format/tests/fixtures/*.sh*; do
      for g in crates/format/tests/fixtures/*.sh*; do
        { cat "$f"; printf '\0'; cat "$g"; printf '\0'; cat "$f"; } >"$dir/$(basename "$f")-$(basename "$g")"
      done
    done
    ;;
  compile | rpc_decode) ;; # libFuzzer starts from the checked-in corpus (or empty)
  *)
    echo "unknown fuzz target: $target" >&2
    exit 2
    ;;
esac
echo "$(find "$dir" -type f | wc -l) corpus inputs in $dir"
