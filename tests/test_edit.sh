#!/bin/sh
set -eu
bin=$(realpath "$(dirname "$0")/../brrr")
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
export XDG_CONFIG_HOME="$tmp"
"$bin" --add make
jq -e '.commands == ["7z","7zz","HandBrakeCLI","blender","c++","cargo","cc","cc1","cc1plus","clang","clang++","ffmpeg","flang","g++","gcc","gfortran","ghb","ghc","go","javac","make","nvcc","obs","resolve","rustc","swift-frontend","swiftc","zig"]' "$tmp/brrr/config.json" >/dev/null
"$bin" --remove cargo
jq -e '.commands == ["7z","7zz","HandBrakeCLI","blender","c++","cc","cc1","cc1plus","clang","clang++","ffmpeg","flang","g++","gcc","gfortran","ghb","ghc","go","javac","make","nvcc","obs","resolve","rustc","swift-frontend","swiftc","zig"]' "$tmp/brrr/config.json" >/dev/null
printf '%s' '{"commands":["rustc"],"other":{"keep":true}}' > "$tmp/brrr/config.json"
"$bin" --add 'a;echo hacked'
jq -e '.commands == ["a;echo hacked", "rustc"] and .other.keep' "$tmp/brrr/config.json" >/dev/null
"$bin" --add gcc & p=$!
"$bin" --add clang & q=$!
wait "$p"; wait "$q"
jq -e '.commands | index("gcc") != null and index("clang") != null' "$tmp/brrr/config.json" >/dev/null
if "$bin" --add /bin/rustc 2>/dev/null; then exit 1; fi
"$bin" --disable
jq -e '.enabled == false and .other.keep and (.commands | index("gcc") != null)' "$tmp/brrr/config.json" >/dev/null
"$bin" --add make
jq -e '.enabled == false and (.commands | index("make") != null)' "$tmp/brrr/config.json" >/dev/null
"$bin" --enable
jq -e '.enabled == true and .other.keep and ([to_entries[] | select(.key == "enabled")] | length) == 1' "$tmp/brrr/config.json" >/dev/null
printf '%s' '{"commands":["rustc"],"enabled":"yes"}' > "$tmp/brrr/config.json"
if "$bin" --add cargo 2>/dev/null; then exit 1; fi
if "$bin" --disable 2>/dev/null; then exit 1; fi
if "$bin" --enable extra 2>/dev/null; then exit 1; fi
printf 'invalid' > "$tmp/brrr/config.json"
if "$bin" --add cargo 2>/dev/null; then exit 1; fi
[ "$(head -c 20 "$tmp/brrr/config.json")" = invalid ]
printf 'whitelist edit tests passed\n'
