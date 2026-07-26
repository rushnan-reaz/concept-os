#!/usr/bin/env bash
# Query the TMP117/TMP102 temperature reading and the DS3231 RTC over the
# bthermo-controller protocol (MQTT channel 11), same wire pattern as
# output_protocol_test.sh. Requires: adapter running, `info` gate passing.
set -euo pipefail

MQTT_HOST="${MQTT_HOST:-127.0.0.1}"
MQTT_PORT="${MQTT_PORT:-1883}"
MQTT_USER="${MQTT_USER:-mqtt}"
MQTT_PASS="${MQTT_PASS:-mqtt}"
ROOT_TOPIC="${ROOT_TOPIC:-bthermo}"
CHANNEL_ID="${CHANNEL_ID:-11}"
REQ_TIMEOUT="${REQ_TIMEOUT:-3}"
TOPIC_IN="${ROOT_TOPIC}/${CHANNEL_ID}/in"
TOPIC_OUT="${ROOT_TOPIC}/${CHANNEL_ID}/out"

pub_hex() {
  local hex="$1"
  echo -n "$hex" | xxd -r -p | mosquitto_pub \
    -h "$MQTT_HOST" -p "$MQTT_PORT" -u "$MQTT_USER" -P "$MQTT_PASS" \
    -t "$TOPIC_IN" -s
}

req_hex() {
  local hex="$1"
  local tmp
  tmp="$(mktemp)"
  (
    timeout "$REQ_TIMEOUT" mosquitto_sub \
      -h "$MQTT_HOST" -p "$MQTT_PORT" -u "$MQTT_USER" -P "$MQTT_PASS" \
      -N -t "$TOPIC_OUT" -C 1 | xxd -p -c 256 > "$tmp"
  ) &
  local subpid=$!
  sleep 0.12
  pub_hex "$hex"
  wait "$subpid" || true
  if [[ -s "$tmp" ]]; then
    cat "$tmp"
  else
    echo "TIMEOUT"
  fi
  rm -f "$tmp"
}

echo "Using topics: in=${TOPIC_IN}, out=${TOPIC_OUT}, timeout=${REQ_TIMEOUT}s"
echo

echo "=== CMD_READ_TEMP (0x74) ==="
temp_hex="$(req_hex 74)"
echo "raw: $temp_hex"
if [[ "$temp_hex" == "TIMEOUT" ]]; then
  echo "No response — controller/sensor not answering (device dead, wrong channel, or bthermo.read_temperature() erroring)."
else
  python3 - "$temp_hex" <<'PY'
import sys, struct
h = sys.argv[1]
b = bytes.fromhex(h)
tag, rest = b[0], b[1:]
if tag == 0x74 and len(rest) == 1:
    # error byte response: [CMD_READ_TEMP, 0x01]
    print(f"  device returned CMD_READ_TEMP error byte: 0x{rest[0]:02x} (sensor read failed / TempNotConnected)")
elif tag == 0x74 and len(rest) == 16*4 + 4:
    history = struct.unpack_from('<16f', rest, 0)
    op_value = struct.unpack_from('<f', rest, 16*4)[0]
    print(f"  history (most recent first, up to 16 samples): {[round(x,3) for x in history]}")
    print(f"  operation_value (aggregate, e.g. current/avg): {op_value:.3f}")
else:
    print(f"  unexpected response shape: tag=0x{tag:02x} len={len(rest)}")
PY
fi

echo
echo "=== CMD_READ_RTC (0x63) ==="
rtc_hex="$(req_hex 63)"
echo "raw: $rtc_hex"
if [[ "$rtc_hex" == "TIMEOUT" ]]; then
  echo "No response — controller/RTC not answering (device dead, wrong channel, or bthermo.read_rtc() erroring)."
else
  python3 - "$rtc_hex" <<'PY'
import sys
h = sys.argv[1]
b = bytes.fromhex(h)
tag, rest = b[0], b[1:]
if tag == 0x63 and len(rest) == 1:
    print(f"  device returned CMD_READ_RTC error byte: 0x{rest[0]:02x} (RTCNotConnected / I2C read failed)")
elif tag == 0x63 and len(rest) == 8:
    sec, minute, hour, week_day, day, month = rest[0:6]
    year = rest[6] | (rest[7] << 8)
    print(f"  {year:04d}-{month:02d}-{day:02d} (weekday={week_day}) {hour:02d}:{minute:02d}:{sec:02d}")
else:
    print(f"  unexpected response shape: tag=0x{tag:02x} len={len(rest)}")
PY
fi

echo
echo "Sanity check: temperature should read a plausible room value (~15-35C)."
echo "RTC should show a real-ish date/time (or at least advance sec-to-sec on repeat runs)."
