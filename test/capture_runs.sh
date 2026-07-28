#!/usr/bin/env bash
# capture_runs.sh — N tail-safe marker captures for ONE operating point.
#
# Each run: (1) reflash the v1 BASE over ST-Link so the device is at v1,
#           (2) start a CONTINUOUS logic capture (can't clip the tail),
#           (3) OTA-update to v2 over the measured transport,
#           (4) wait so INSTALL/MIGRATE fire, then stop the capture,
#           (5) append a metadata row to the manifest.
#
# Usage:  ./capture_runs.sh <LABEL> <N>
#   e.g.  ./capture_runs.sh VCP_115200 5
#
# Edit the CONFIG block for each operating point before running.
set -u

ROOT="/home/rushnan-reaz/concept-os"
cd "$ROOT" || { echo "no repo at $ROOT"; exit 1; }

# ============================ CONFIG ============================
LABEL="${1:-VCP_115200}"     # operating-point name -> subdir + manifest
N="${2:-5}"                  # number of runs

HBF="$ROOT/components/bthermo/core/bthermo.hbf"   # the v2 component pushed over OTA

# --- BASE reflash command (wired, ST-Link). Puts the device back at v1. ---
# Build the base image ONCE first:  ./test/flash_output_test_vcp.sh
# Thereafter this re-flashes the already-built image (fast). Adjust --image-path
# / --app-config to match the transport you're measuring (VCP vs BT app config).
APP_CFG="$ROOT/app/bthermo/App.toml"
BASE_IHEX="$ROOT/app/bthermo/App.ihex"
RESET_CMD=( ./toolchain/modules/update_tool/update-tool-uart flash-system
            --app-config "$APP_CFG" --image-path "$BASE_IHEX" )

# --- OTA update command (the MEASURED transport). ---
# Firmware runs with the "multi-support" framing, which only the mqtt_adapter
# bridge implements — update-tool-uart talking to the raw serial port directly
# does NOT speak this framing and will hang. Always go through the bridge:
#   VCP: mqtt_adapter on settings_vcp.yaml (root_topic bthermo_vcp, /dev/ttyACM0)
#   BT:  mqtt_adapter on settings.yaml     (root_topic bthermo,     /dev/rfcomm0)
# Start the matching bridge before running this script.
OTA_CMD=( ./toolchain/modules/update_tool/update-tool-mqtt
          -i 127.0.0.1 -p 1883 -t bthermo -v true
          flash-component -f "$HBF" )

SR_CHANNELS="D2,D3,D4,D5,D6,D7"
SR_RATE="2m"
BOOT_SETTLE=5                # seconds after reflash for v1 to boot + transport ready
CAP_LEAD=2                   # seconds of capture before firing the OTA
TAIL_SETTLE=3                # seconds after "Success!" so INSTALL/MIGRATE land
# sigrok-cli --continuous reads a keypress from stdin to know when to stop;
# backgrounded/non-interactive it either gets SIGTTIN-stopped or exits
# immediately on stdin EOF, producing an empty capture either way. --time
# mode never touches stdin, so it's the reliable choice here. Duration must
# comfortably exceed CAP_LEAD + slowest expected RECV + TAIL_SETTLE (BT@9600
# RECV is ~30s) — pad generously since a short capture silently clips the tail.
CAP_TIME=60                  # total capture duration in seconds
# ===============================================================

OUTDIR="$ROOT/analysis/output/ConceptOS/${LABEL}"
mkdir -p "$OUTDIR"
MANIFEST="${OUTDIR}/manifest.csv"
[ -f "$MANIFEST" ] || echo "run,label,hbf_bytes,ota_cmd,timestamp,sr_file" > "$MANIFEST"
HBF_BYTES=$(stat -c%s "$HBF" 2>/dev/null || echo "?")

for i in $(seq 1 "$N"); do
  run=$(printf "run%02d" "$i")
  sr="${OUTDIR}/${run}.sr"
  echo "================ $LABEL / $run ================"

  echo "[1/4] reflash v1 base ..."
  if ! "${RESET_CMD[@]}"; then
    echo "  !! base reflash failed — skipping $run"; continue
  fi
  sleep "$BOOT_SETTLE"
  # If measuring over Bluetooth, a device reset can drop rfcomm — re-bind here
  # if needed, e.g.:  sudo rfcomm connect /dev/rfcomm0 <MAC> 1 &

  echo "[2/4] start capture -> $sr"
  pkill -i sigrok 2>/dev/null; sleep 0.5
  sigrok-cli -d fx2lafw --config samplerate="$SR_RATE" -C "$SR_CHANNELS" \
    --time "${CAP_TIME}s" -o "$sr" &
  SGPID=$!
  sleep "$CAP_LEAD"

  echo "[3/4] OTA update to v2 ..."
  "${OTA_CMD[@]}"

  echo "[4/4] settle + wait for capture to finish"
  sleep "$TAIL_SETTLE"
  wait "$SGPID" 2>/dev/null

  ts=$(date -Iseconds)
  echo "${run},${LABEL},${HBF_BYTES},\"${OTA_CMD[*]}\",${ts},${sr}" >> "$MANIFEST"
  echo "  saved $sr"
  sleep 1
done

echo
echo "Done: $N runs -> $OUTDIR"
echo "Aggregate with:  python3 aggregate_runs.py $OUTDIR"
