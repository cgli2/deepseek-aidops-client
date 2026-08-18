#!/usr/bin/env bash
# ============================================================================
#  One-click build / package script for the harness workspace (Linux / macOS).
#
#  On Windows the GNU toolchain needs MinGW's `dlltool.exe`, which is missing,
#  so use scripts/build.bat instead — it initialises the MSVC environment and
#  builds for the x86_64-pc-windows-msvc target.
#
#  USAGE
#  ------
#      ./scripts/build.sh                              # cargo build
#      ./scripts/build.sh check -p harness-bin --features gui
#      ./scripts/build.sh package                      # release build + dist/
# ============================================================================
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

# On Windows (Git Bash / MSYS / Cygwin) delegate to the .bat that sets up MSVC.
if [[ "$OSTYPE" == "msys"* || "$OSTYPE" == "mingw"* || "$OSTYPE" == "cygwin"* ]]; then
  echo "[build] Windows detected -> delegating to build.bat"
  exec cmd.exe /c "$(cygpath -w "$SCRIPT_DIR/build.bat")" "$@"
fi

# On Linux / macOS the native target works out of the box.
cd "$SCRIPT_DIR/.."

if [[ "${1:-}" == "package" ]]; then
  echo "[build] cargo build --release --all-features"
  cargo build --release --all-features
  mkdir -p dist
  if [[ -f "target/release/aidops-desktop" ]]; then
    cp "target/release/aidops-desktop" dist/
  else
    cp "target/release/aidops-desktop.exe" dist/
  fi
  [[ -f config/default.toml ]] && cp config/default.toml dist/
  [[ -f extensions/EXTENSION-COOKBOOK.md ]] && cp extensions/EXTENSION-COOKBOOK.md dist/
  echo "[build] packaged -> dist/aidops-desktop (+ config/default.toml, extensions/EXTENSION-COOKBOOK.md)"
else
  cargo "$@"
fi
