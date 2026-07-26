#!/usr/bin/env bash
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

# Same packet layout used by bthermo_app (date, date, setpoint f32-le, output, repeat)
add_force_program_for_output() {
  local out_num="$1"
  local out_hex
  printf -v out_hex "%02x" "$out_num"

  # from_time: 00:00:00 weekday=1 day=1 month=1 year=2024
  # to_time:   23:59:59 weekday=1 day=1 month=1 year=2024
  # setpoint:  99.0C (00 00 c6 42)
  # output:    01..04
  # repeat:    01 (EveryDay)
  local payload="000000010101e8073b3b17010101e8070000c642${out_hex}01"

  pub_hex "6e"      # CMD_SET_PROGRAM
  req_hex "$payload" # expects 6e00
}

remove_program_id() {
  local id="$1"
  local id_hex
  printf -v id_hex "%02x" "$id"
  pub_hex "64"      # CMD_REM_PROGRAM
  req_hex "$id_hex"
}

clear_programs() {
  local id
  for id in $(seq 0 31); do
    local r
    r="$(remove_program_id "$id")"
    case "$r" in
      6400)
        echo "removed program id $id"
        ;;
      6401)
        # Program slot already empty.
        ;;
      TIMEOUT)
        echo "warn: timeout removing program id $id" >&2
        ;;
      *)
        echo "warn: unexpected remove response for id $id: $r" >&2
        ;;
    esac
  done

  return 0
}

echo "Using topics: in=${TOPIC_IN}, out=${TOPIC_OUT}, timeout=${REQ_TIMEOUT}s"

echo "=== Baseline output status ==="
base="$(req_hex 6f)"
echo "outputs reply: $base"

if [[ "$base" == "TIMEOUT" ]]; then
  echo "No protocol response. Ensure adapter + serial path are online first."
  exit 2
fi

echo "=== Clearing existing programs ==="
clear_programs

for out in 1 2 3 4; do
  echo
  echo "=== Force OUT${out} ON (active-low pin) ==="
  clear_programs >/dev/null
  set_resp="$(add_force_program_for_output "$out")"
  out_resp="$(req_hex 6f)"
  echo "set-program response: $set_resp"
  echo "outputs response:     $out_resp"
done

echo
echo "Done. Probe pins while each OUT test runs:"
echo "OUT1->PB5(D4), OUT2->PB4(D5), OUT3->PB10(D6), OUT4->PB6(D10)"
