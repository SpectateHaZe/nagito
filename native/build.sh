#!/usr/bin/env bash
# Builds native/build/nagito_rp.dll inside MSYS2 UCRT64 and copies it plus its DLL deps to $1.
set -euo pipefail
export MSYSTEM=UCRT64 PATH=/ucrt64/bin:/usr/bin:$PATH
here="$(cd "$(dirname "$0")" && pwd)"
cmake -S "$here" -B "$here/build" -G Ninja -DCMAKE_BUILD_TYPE=Release >/dev/null
cmake --build "$here/build" --target nagito_rp
out="${1:-$here/build}"
mkdir -p "$out"
cp -u "$here/build/libnagito_rp.dll" "$out/"
# copy every non-system DLL it depends on
ldd "$here/build/libnagito_rp.dll" | awk 'tolower($3) !~ /\/windows\// && $3 ~ /^\// {print $3}' | sort -u | xargs -r -I{} cp -u {} "$out/"