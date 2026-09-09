#!/usr/bin/env bash
# Task E: push each of the 6 churn-variant deltas over real OTA (VCP@115200),
# single run each, with delta phase-marker capture (D2-D7 = PC2-PC7).
set -u

ROOT="/home/rushnan-reaz/concept-os"
cd "$ROOT" || exit 1

MQTT_TOPIC="${MQTT_TOPIC:-bthermo_vcp}"
APP_CFG="$ROOT/app/bthermo/App.vcp.toml"
BASE_IHEX="$ROOT/test/output_delta_vcp/App.ihex"

OUTDIR="$ROOT/analysis/output/delta_singlewrite/task_e"
mkdir -p "$OUTDIR"

VARIANTS="in_place_tiny in_place_medium in_place_large size_change_small size_change_medium size_change_large"

for variant in $VARIANTS; do
  delta="$ROOT/test/churn_variants/deltas/${variant}.delta.hbf"
  sr="${OUTDIR}/${variant}.sr"
  echo "================ $variant ================"

  echo "[1/4] Reflashing v1 baseline"
  ./toolchain/modules/update_tool/update-tool-uart flash-system \
    --app-config "$APP_CFG" --image-path "$BASE_IHEX" > /tmp/flash.log 2>&1
  if ! grep -q "Success!" /tmp/flash.log; then echo "flash failed"; cat /tmp/flash.log; continue; fi

  echo "[2/4] Settle + confirm v1 alive"
  sleep 5
  timeout 20 ./toolchain/modules/update_tool/update-tool-mqtt -i 127.0.0.1 -p 1883 -t "$MQTT_TOPIC" -v false info > /tmp/info_before.log 2>&1
  if ! grep -q "Version: 1" /tmp/info_before.log; then echo "v1 not confirmed alive, skipping"; cat /tmp/info_before.log; continue; fi

  echo "[3/4] Capture + push $variant delta ($(stat -c%s "$delta") bytes)"
  sigrok-cli -d fx2lafw --config samplerate=2m -C D0,D1,D2,D3,D4,D5,D6,D7 --time 20s -o "$sr" &
  SGPID=$!
  sleep 2
  timeout 15 ./toolchain/modules/update_tool/update-tool-mqtt -i 127.0.0.1 -p 1883 -t "$MQTT_TOPIC" -v false flash-component -f "$delta" > /tmp/ota_push.log 2>&1
  wait "$SGPID"
  tail -c 300 /tmp/ota_push.log | tr '\r' '\n' | tail -3

  echo "[4/4] Confirm result"
  timeout 20 ./toolchain/modules/update_tool/update-tool-mqtt -i 127.0.0.1 -p 1883 -t "$MQTT_TOPIC" -v false info > /tmp/info_after.log 2>&1
  grep -A2 "Component ID: 10" /tmp/info_after.log
  echo "  saved $sr"
done

echo
echo "Done: 6 variants -> $OUTDIR"
