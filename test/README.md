# Output-Only Flash/Test Harness

This folder contains a practical test harness that reuses the **same pins and protocols** already used by the `bthermo` app:

- Output pins (active-low):
  - OUT1 -> PB5 (D4)
  - OUT2 -> PB4 (D5)
  - OUT3 -> PB10 (D6)
  - OUT4 -> PB6 (D10)
- Controller protocol on MQTT channel `11` (`bthermo/11/in` and `bthermo/11/out`)
  - `0x6f` -> get outputs
  - `0x6e` -> set program
  - `0x64` -> remove program

## Files

- `flash_output_test.sh`: builds and flashes a full image using the same Concept-OS flow.
- `flash_output_test_vcp.sh`: builds and flashes a temporary image that swaps `uart3-channel` with `uart2-channel` (USART2 over ST-Link VCP).
- `flash_component_mqtt.sh`: updates a single component `.cbf` (or `.hbf`) over MQTT (checks `info` before and after flashing).
- `output_protocol_test.sh`: output-only protocol test (clears programs, forces one output at a time ON, reads status).

## Quick Start

1. Flash firmware:

```bash
cd /home/rushnan-reaz/concept-os/test
./flash_output_test.sh

# Or flash temporary USART2 (ST-Link VCP) transport variant:
./flash_output_test_vcp.sh
```

2. Start MQTT adapter (in a separate terminal):

```bash
cd /home/rushnan-reaz/concept-os/utils/mqtt_adapter
./venv/bin/python main.py -c settings.yaml
```

3. Run output protocol test:

```bash
cd /home/rushnan-reaz/concept-os/test
CHANNEL_ID=11 ./output_protocol_test.sh
```

4. Update one component over MQTT:

```bash
cd /home/rushnan-reaz/concept-os/test
./flash_component_mqtt.sh /path/to/component.cbf
```

Example with current repo artifacts:

```bash
./flash_component_mqtt.sh /home/rushnan-reaz/concept-os/components/bthermo/core/bthermo.cbf
```

## Notes

- This harness uses the current `app/bthermo/App.toml` and `update-tool-uart flash-system` path.
- The temporary VCP variant uses `app/bthermo/App.vcp.toml`, where only the transport component is swapped to `uart2-channel` (protocol and channel behavior kept equivalent).
- `update-tool-mqtt` flag name is `--hbf-file`, but `.cbf` artifacts are accepted as long as the binary format validates.
- `flash_component_mqtt.sh` now exits non-zero if updater output reports protocol-level errors (for example `IllegalDowngrade`).
- If all commands timeout, fix transport first (`/dev/ttyACM0`, single adapter instance, broker alive).

## Detailed Session Worklog

- See `test/doc/vcp-debug-worklog-2026-04-17.md` for a full step-by-step record of changes, rationale, and code snippets.
