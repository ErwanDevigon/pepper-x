#!/bin/bash
# Release build of the three workspace binaries, inside this tree.
# PATH wrappers cap ninja -j and nvcc --threads. CARGO_TARGET_DIR stays target/.
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
export PATH="$root/scripts/build-limit:$PATH"
export PEPPERX_CMAKE_JOBS="${PEPPERX_CMAKE_JOBS:-4}"
cd "$root"
exec cargo build --release \
  -p pepper-x-app \
  -p pepperx-cleanup-helper \
  -p pepperx-uinput-helper \
  "$@"
