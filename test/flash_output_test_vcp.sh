#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="/home/rushnan-reaz/concept-os"
APP_CFG="$ROOT_DIR/app/bthermo/App.vcp.toml"
OUT_BASE="$ROOT_DIR/test/output_test_app_vcp/App"
OUT_DIR="$(dirname "$OUT_BASE")"

cd "$ROOT_DIR"

mkdir -p "$OUT_DIR"
if [[ ! -d "$OUT_DIR" ]]; then
  echo "Cannot create output directory: $OUT_DIR" >&2
  exit 1
fi

echo "[1/4] Building system image with temporary USART2 transport (ST-Link VCP)..."
./toolchain/modules/system_builder/system_builder \
  --app-config "$APP_CFG" \
  --output-path "$OUT_BASE"

echo "[2/4] Checking generated image files..."
ls -lh "$OUT_DIR"/App.{elf,ihex,srec,bin}

if [[ "${SKIP_FLASH:-0}" == "1" ]]; then
  echo "[3/4] SKIP_FLASH=1, skipping OpenOCD flash step."
  echo "[4/4] Build-only completed."
  exit 0
fi

echo "[3/4] Flashing full system via ST-Link/OpenOCD..."
./toolchain/modules/update_tool/update-tool-uart flash-system \
  --app-config "$APP_CFG" \
  --image-path "$OUT_DIR/App.ihex"

echo "[4/4] Done. Use mqtt_adapter on ST-Link VCP and run output_protocol_test.sh"
