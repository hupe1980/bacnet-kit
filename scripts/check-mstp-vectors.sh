#!/bin/sh
# Rebuild the capture fixtures with an independent bacnet-stack checkout.
set -eu
peer=$(cd "${1:?supply the pinned bacnet-stack checkout}" && pwd)
kit=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
cmake -S "$peer" -B "$work/build" -DBACDL_MSTP=ON -DBACDL_BIP=OFF -DBACNET_STACK_BUILD_APPS=OFF
cmake --build "$work/build" --target bacnet-stack --parallel 4
"${CC:-cc}" -std=c11 -Wall -Wextra -Werror -I"$peer/src" "$kit/checks/mstp-vectors.c" \
    "$work/build/libbacnet-stack.a" -o "$work/vectors"
(cd "$work" && ./vectors)
for name in standard-501 extended-502 extended-1497; do
    cmp "$work/$name.bin" "$kit/tests/fixtures/mstp/$name.bin"
    echo "$name matched the independent C encoder"
done
