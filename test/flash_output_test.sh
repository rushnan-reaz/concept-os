#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="/home/rushnan-reaz/concept-os"
APP_CFG="$ROOT_DIR/app/bthermo/App.toml"
OUT_BASE="$ROOT_DIR/test/output_test_app/App"
OUT_DIR="$(dirname "$OUT_BASE")"

cd "$ROOT_DIR"

mkdir -p "$OUT_DIR"
if [[ ! -d "$OUT_DIR" ]]; then
  echo "Cannot create output directory: $OUT_DIR" >&2
  exit 1
fi

echo "[1/3] Building system image (same flow as app/bthermo make build)..."
./toolchain/modules/system_builder/system_builder \
  --app-config "$APP_CFG" \
  --output-path "$OUT_BASE"

echo "[2/3] Checking generated image files..."
ls -lh "$OUT_DIR"/App.{elf,ihex,srec,bin}

echo "[3/3] Flashing full system via ST-Link/OpenOCD..."
./toolchain/modules/update_tool/update-tool-uart flash-system \
  --app-config "$APP_CFG" \
  --image-path "$OUT_DIR/App.ihex"

echo "Done. Firmware flashed from test harness output path."
