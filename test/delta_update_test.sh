#!/usr/bin/env bash
# Delta component update end-to-end test.
#
# Generates a Delta HBF from a pristine (old.hbf, new.hbf) pair (or uses a
# prebuilt delta) and pushes it to the device via the existing `flash-component`
# path, which auto-detects IS_DELTA and drives the delta protocol
# (0x01 -> 0xB0 -> 0xA0...). Mirrors flash_component_mqtt.sh but delta-aware.
#
# Usage:
#   ./delta_update_test.sh <old.hbf> <new.hbf>        # generate + flash a delta
#   ./delta_update_test.sh --delta <delta.hbf>        # flash a prebuilt delta
#
# Transport (default MQTT, matching flash_component_mqtt.sh):
#   TRANSPORT=mqtt  MQTT_HOST=127.0.0.1 MQTT_PORT=1883 ROOT_TOPIC=bthermo
#   TRANSPORT=uart  SERIAL_PORT=/dev/ttyACM0
#
# Prereqs: base system already flashed (test/flash_output_test.sh) and, for MQTT,
# the adapter running (utils/mqtt_adapter). The flashed base component MUST be
# the same build as <old.hbf>, or the reconstructed-CRC gate will (safely) abort.
set -euo pipefail

ROOT_DIR="/home/rushnan-reaz/concept-os"
DELTA_GEN="$ROOT_DIR/toolchain/modules/delta_gen/target/debug/delta_gen"
TRANSPORT="${TRANSPORT:-mqtt}"
VERBOSE="${VERBOSE:-true}"
TIMEOUT_SECS="${TIMEOUT_SECS:-60}"

# --- Parse args -------------------------------------------------------------
DELTA_FILE=""
if [[ "${1:-}" == "--delta" ]]; then
  DELTA_FILE="${2:-}"
  [[ -f "$DELTA_FILE" ]] || { echo "Delta not found: $DELTA_FILE" >&2; exit 1; }
else
  OLD_HBF="${1:-}"; NEW_HBF="${2:-}"
  if [[ -z "$OLD_HBF" || -z "$NEW_HBF" ]]; then
    echo "Usage: $0 <old.hbf> <new.hbf>   |   $0 --delta <delta.hbf>" >&2
    exit 1
  fi
  [[ -f "$OLD_HBF" ]] || { echo "old.hbf not found: $OLD_HBF" >&2; exit 1; }
  [[ -f "$NEW_HBF" ]] || { echo "new.hbf not found: $NEW_HBF" >&2; exit 1; }
fi

cd "$ROOT_DIR"

# --- Build delta_gen if needed ---------------------------------------------
if [[ ! -x "$DELTA_GEN" ]]; then
  echo "[build] delta_gen"
  (cd toolchain/modules/delta_gen && cargo build)
fi

# --- Generate the Delta HBF (also runs the in-memory self-check) ------------
if [[ -z "$DELTA_FILE" ]]; then
  DELTA_FILE="/tmp/$(basename "${NEW_HBF%.hbf}")_delta.hbf"
  echo "[1/4] Generating Delta HBF -> $DELTA_FILE"
  "$DELTA_GEN" "$OLD_HBF" "$NEW_HBF" -o "$DELTA_FILE"
  echo
else
  echo "[1/4] Using prebuilt Delta HBF: $DELTA_FILE"
fi

# --- Select transport / update tool ----------------------------------------
case "$TRANSPORT" in
  mqtt)
    TOOL="$ROOT_DIR/toolchain/modules/update_tool/update-tool-mqtt"
    COMMON=(-i "${MQTT_HOST:-127.0.0.1}" -p "${MQTT_PORT:-1883}" -t "${ROOT_TOPIC:-bthermo}" -v "$VERBOSE")
    ;;
  uart)
    TOOL="$ROOT_DIR/toolchain/modules/update_tool/update-tool-uart"
    COMMON=(-s "${SERIAL_PORT:-/dev/ttyACM0}")
    ;;
  *)
    echo "Unknown TRANSPORT: $TRANSPORT (use mqtt|uart)" >&2; exit 1 ;;
esac
if [[ ! -x "$TOOL" ]]; then
  echo "update tool not built: $TOOL" >&2
  echo "Build it: (cd toolchain/modules/update_tool && make build)" >&2
  exit 1
fi

# --- info -> flash-component (delta) -> info --------------------------------
echo "[2/4] Device status BEFORE update"
timeout "$TIMEOUT_SECS" "$TOOL" "${COMMON[@]}" info || true

echo "[3/4] Pushing delta via flash-component"
flash_log="$(mktemp)"
set +e
timeout "$TIMEOUT_SECS" "$TOOL" "${COMMON[@]}" flash-component -f "$DELTA_FILE" 2>&1 | tee "$flash_log"
flash_rc=${PIPESTATUS[0]}
set -e
if [[ $flash_rc -ne 0 ]]; then
  echo "Delta update command failed (exit $flash_rc)" >&2
  rm -f "$flash_log"; exit "$flash_rc"
fi
if grep -Eq "Unexpected response from device|IllegalDowngrade|FailedHBFValidation|CannotReadHBF|NotEnoughSpace|DeltaBaseNotFound|DeltaBaseCrcMismatch|DeltaReconstructMismatch" "$flash_log"; then
  echo "Delta update failed: device reported a protocol-level error." >&2
  echo "  DeltaBaseNotFound        -> no installed component matches the delta's base id+version." >&2
  echo "  DeltaBaseCrcMismatch     -> installed base is the wrong build (masked base-CRC failed)." >&2
  echo "  DeltaReconstructMismatch -> reconstructed image CRC failed (corrupt patch/transfer)." >&2
  rm -f "$flash_log"; exit 3
fi
rm -f "$flash_log"

echo "[4/4] Device status AFTER update"
timeout "$TIMEOUT_SECS" "$TOOL" "${COMMON[@]}" info

echo "Done. Delta update completed via $TRANSPORT."
