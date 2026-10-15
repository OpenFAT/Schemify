#!/usr/bin/env bash
# Seeds a fuzz corpus directory (nightly `fuzz` and `fuzz-corpus` jobs), from the repository root:
#
#   tests/fuzz/seeds.sh <target> <corpus dir> [--fetched]
#
# Seeds are this repository's own files. `--fetched` adds the corpus sources `fetch.sh`
# downloads (xschem_read: real PDK and xschem library files, docs/dev/testing.md); those include GPL
# xschem data, so a corpus seeded with them is never committed (the `fuzz-corpus` job omits it).
set -eu # no pipefail: `head` closes the find pipe early on purpose

target="$1"
dir="$2"
fetched="${3:-}"
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
  xschem_read)
    roots="tests/determinism/xschem"
    if [ "$fetched" = --fetched ]; then
      tests/corpus/fetch.sh >/dev/null
      roots="$roots tests/corpus/.cache/src"
    fi
    # shellcheck disable=SC2086 # roots is a list of paths without spaces
    find $roots \( -name '*.sch' -o -name '*.sym' \) -type f -size -64k | head -n 4000 |
      while read -r f; do cp "$f" "$dir/$(echo "$f" | sha1sum | cut -c1-16)"; done
    ;;
  compile | rpc_decode) ;; # libFuzzer starts from the checked-in corpus (or empty)
  *)
    echo "unknown fuzz target: $target" >&2
    exit 2
    ;;
esac
echo "$(find "$dir" -type f | wc -l) corpus inputs in $dir"
