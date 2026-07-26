#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="/home/rushnan-reaz/concept-os"
UPDATE_TOOL="$ROOT_DIR/toolchain/modules/update_tool/update-tool-mqtt"

MQTT_HOST="${MQTT_HOST:-127.0.0.1}"
MQTT_PORT="${MQTT_PORT:-1883}"
ROOT_TOPIC="${ROOT_TOPIC:-bthermo}"
VERBOSE="${VERBOSE:-false}"
TIMEOUT_SECS="${TIMEOUT_SECS:-40}"

COMP_FILE="${COMP_FILE:-${HBF_FILE:-${1:-}}}"

usage() {
  echo "Usage: $0 <component.cbf|component.hbf>"
  echo
  echo "Environment overrides:"
  echo "  MQTT_HOST=127.0.0.1"
  echo "  MQTT_PORT=1883"
  echo "  ROOT_TOPIC=bthermo"
  echo "  VERBOSE=false"
  echo "  TIMEOUT_SECS=40"
  echo "  COMP_FILE=/abs/or/rel/path/to/component.cbf"
  echo "  HBF_FILE=/abs/or/rel/path/to/component.hbf  # legacy alias"
}

if [[ -z "$COMP_FILE" ]]; then
  usage
  exit 1
fi

cd "$ROOT_DIR"

if [[ ! -f "$COMP_FILE" ]]; then
  echo "Component file not found: $COMP_FILE" >&2
  exit 1
fi

case "$COMP_FILE" in
  *.cbf|*.hbf)
    ;;
  *)
    echo "Warning: expected .cbf/.hbf component file, got: $COMP_FILE" >&2
    ;;
esac

if [[ ! -x "$UPDATE_TOOL" ]]; then
  echo "update-tool-mqtt not found or not executable: $UPDATE_TOOL" >&2
  echo "Build it first from toolchain/modules/update_tool if needed." >&2
  exit 1
fi

echo "[1/3] Checking MQTT update path (info)..."
timeout "$TIMEOUT_SECS" "$UPDATE_TOOL" \
  -i "$MQTT_HOST" -p "$MQTT_PORT" -t "$ROOT_TOPIC" -v "$VERBOSE" \
  info

echo "[2/3] Flashing component via MQTT..."
flash_log="$(mktemp)"
set +e
timeout "$TIMEOUT_SECS" "$UPDATE_TOOL" \
  -i "$MQTT_HOST" -p "$MQTT_PORT" -t "$ROOT_TOPIC" -v "$VERBOSE" \
  flash-component -f "$COMP_FILE" 2>&1 | tee "$flash_log"
flash_rc=${PIPESTATUS[0]}
set -e

if [[ $flash_rc -ne 0 ]]; then
  echo "Component update command failed with exit code $flash_rc" >&2
  rm -f "$flash_log"
  exit "$flash_rc"
fi

if grep -Eq "Unexpected response from device|IllegalDowngrade|FailedHBFValidation|CannotReadHBF" "$flash_log"; then
  echo "Component update failed: updater reported a protocol-level error." >&2
  rm -f "$flash_log"
  exit 3
fi

rm -f "$flash_log"

echo "[3/3] Reading status after update..."
timeout "$TIMEOUT_SECS" "$UPDATE_TOOL" \
  -i "$MQTT_HOST" -p "$MQTT_PORT" -t "$ROOT_TOPIC" -v "$VERBOSE" \
  info

echo "Done. Component update over MQTT completed."