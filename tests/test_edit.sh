#!/bin/sh
set -eu
bin=$(realpath "$(dirname "$0")/../omapower")
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
export XDG_CONFIG_HOME="$tmp"
"$bin" --add make
jq -e '.commands == ["c++","cargo","cc","cc1","cc1plus","clang","clang++","flang","g++","gcc","gfortran","ghc","go","javac","make","nvcc","rustc","swift-frontend","swiftc","zig"]' "$tmp/omapower/config.json" >/dev/null
"$bin" --remove cargo
jq -e '.commands == ["c++","cc","cc1","cc1plus","clang","clang++","flang","g++","gcc","gfortran","ghc","go","javac","make","nvcc","rustc","swift-frontend","swiftc","zig"]' "$tmp/omapower/config.json" >/dev/null
printf '%s' '{"commands":["rustc"],"other":{"keep":true}}' > "$tmp/omapower/config.json"
"$bin" --add 'a;echo hacked'
jq -e '.commands == ["a;echo hacked", "rustc"] and .other.keep' "$tmp/omapower/config.json" >/dev/null
"$bin" --add gcc & p=$!
"$bin" --add clang & q=$!
wait "$p"; wait "$q"
jq -e '.commands | index("gcc") != null and index("clang") != null' "$tmp/omapower/config.json" >/dev/null
if "$bin" --add /bin/rustc 2>/dev/null; then exit 1; fi
"$bin" --disable
jq -e '.enabled == false and .other.keep and (.commands | index("gcc") != null)' "$tmp/omapower/config.json" >/dev/null
"$bin" --add make
jq -e '.enabled == false and (.commands | index("make") != null)' "$tmp/omapower/config.json" >/dev/null
"$bin" --enable
jq -e '.enabled == true and .other.keep and ([to_entries[] | select(.key == "enabled")] | length) == 1' "$tmp/omapower/config.json" >/dev/null
printf '%s' '{"commands":["rustc"],"enabled":"yes"}' > "$tmp/omapower/config.json"
if "$bin" --add cargo 2>/dev/null; then exit 1; fi
if "$bin" --disable 2>/dev/null; then exit 1; fi
if "$bin" --enable extra 2>/dev/null; then exit 1; fi
printf 'invalid' > "$tmp/omapower/config.json"
if "$bin" --add cargo 2>/dev/null; then exit 1; fi
[ "$(head -c 20 "$tmp/omapower/config.json")" = invalid ]
printf 'whitelist edit tests passed\n'
