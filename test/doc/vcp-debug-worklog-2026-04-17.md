# Concept-OS VCP Debug Worklog (2026-04-17)

This document records what I changed in this session, step by step, including the reason for each change and the relevant code.

## Scope and intent

Goal of this work:
- Build and flash a temporary firmware path that uses ST-Link VCP (USART2) instead of Bluetooth/rfcomm.
- Verify protocol/runtime communication end-to-end.
- Stabilize the test scripts used during validation.

Notes:
- I focus on manually edited code/config/scripts.
- Generated artifacts (App.ihex/App.bin, reports, binary tools) are not listed as hand-edited changes.

---

## Step 1: Create a temporary app config for VCP transport

Why changed:
- Needed a dedicated app config to swap transport to `uart2-channel` for ST-Link VCP testing without changing the normal app path.

File changed:
- `app/bthermo/App.vcp.toml` (new)

Code added:
```toml
name = "bthermo"
board = "stm32l476rg"
kernel_ram = 6656
clock_speed = 80000000
strip_panics = false

# Temporary transport swap for ST-Link VCP tests
[components.uart2-channel]
features = ["log-itm", "multi-support"]

[components.update]
features = ["log-itm", "multi-support"]
```

---

## Step 2: Add temporary USART2 transport component (`uart2-channel`)

Why changed:
- The existing test/debug path needed UART over ST-Link VCP (USART2 on PA2/PA3, DMA1 CH6 RX) while keeping the same packet protocol and task API style.

Files changed:
- `components/uart2-channel/api/Cargo.toml` (new)
- `components/uart2-channel/api/src/lib.rs` (new)
- `components/uart2-channel/core/Cargo.toml` (new)
- `components/uart2-channel/core/Component.toml` (new)
- `components/uart2-channel/core/src/main.rs` (new)
- supporting `.gitignore` / `Makefile` files for this component path

Key code snippets:

API task binding (`TaskId(3)`):
```rust
const UART_CHANNEL_ID: TaskId = TaskId(3);
```

USART2 GPIO setup (PA2/PA3 AF7):
```rust
#[cfg(any(feature = "board_stm32f303re", feature = "board_stm32l476rg"))]
fn setup_gpio() -> Result<(), RCCError> {
    let gpioa = unsafe { &*device::GPIOA::ptr() };
    let mut rcc = rcc_api::RCC::new();
    rcc.enable_clock(rcc_api::Peripheral::GPIOA)?;
    rcc.leave_reset(rcc_api::Peripheral::GPIOA)?;

    gpioa
        .moder
        .modify(|_, w| w.moder2().alternate().moder3().alternate());
    gpioa.afrl.modify(|_, w| w.afrl2().af7().afrl3().af7());
    Ok(())
}
```

L476 DMA routing for USART2_RX on channel 6:
```rust
#[cfg(feature = "board_stm32l476rg")]
fn configure_dma_rx(dma1: &device::dma1::RegisterBlock, usart: &device::usart1::RegisterBlock) {
    dma1.ccr6.modify(|_, w| w.en().clear_bit());
    dma1.ifcr.write(|w| w.cgif6().set_bit());

    // Select USART2_RX for Channel 6
    dma1.cselr.modify(|_, w| w.c6s().bits(0b0010));

    dma1.cpar6.write(|w| unsafe { w.bits(usart.rdr.as_ptr() as u32) });
    dma1.cmar6.write(|w| unsafe { w.bits(RX_BUFFER.as_mut_ptr() as u32) });
    dma1.cndtr6.write(|w| unsafe { w.bits(RX_BUFFER_SIZE as u32) });
    // ...
}
```

---

## Step 3: Add VCP flash script for build/flash loop

Why changed:
- Needed a repeatable one-command path to build and flash the VCP variant.
- Also needed a build-only mode and safer output dir handling.

File changed:
- `test/flash_output_test_vcp.sh`

Key code:
```bash
mkdir -p "$OUT_DIR"
if [[ ! -d "$OUT_DIR" ]]; then
  echo "Cannot create output directory: $OUT_DIR" >&2
  exit 1
fi

if [[ "${SKIP_FLASH:-0}" == "1" ]]; then
  echo "[3/4] SKIP_FLASH=1, skipping OpenOCD flash step."
  echo "[4/4] Build-only completed."
  exit 0
fi
```

---

## Step 4: Harden MQTT adapter topic parsing and runtime setup

Why changed:
- Topic parsing needed stricter matching and proper root escaping.
- Static MQTT client ID could collide with stale processes.
- Event loop handling needed explicit `new_event_loop` setup for stable runtime behavior.

File changed:
- `utils/mqtt_adapter/main.py`

Key code changes:

Topic regex hardening:
```python
match = re.match(rf"{re.escape(mqtt_root)}/([^/]+)/in$", topic)
```

Client ID made configurable with pid-based default:
```python
mqtt_client = MQTTConnector(
    client_id=settings.get('mqtt/client_id', default_value=f"concept-os-adapter-{os.getpid()}"),
    will_topic=mqtt_root + "/available",
    will_offline_payload='0'
)
```

Event loop creation/cleanup:
```python
loop = aio.new_event_loop()
aio.set_event_loop(loop)
loop.create_task(init(settings=settings))
# ...
aio.set_event_loop(None)
loop.close()
```

---

## Step 5: Point adapter settings to ST-Link VCP + local broker

Why changed:
- During VCP testing we needed serial to bind to ST-Link by-id path and MQTT to local broker.

File changed:
- `utils/mqtt_adapter/settings.yaml`

Key code:
```yaml
serial:
  # port_name: /dev/rfcomm0
  port_name: /dev/serial/by-id/usb-STMicroelectronics_STM32_STLink_02470215523200063638414B-if02
  baudrate: 115200

mqtt:
  server_ip: "127.0.0.1"
```

---

## Step 6: Fix protocol test receiver handling (`mosquitto_sub -N`)

Why changed:
- `mosquitto_sub` default output can append newline behavior that corrupts hex parsing in this script context.
- `-N` removes the extra newline path and keeps payload handling cleaner.

File changed:
- `test/output_protocol_test.sh`

Key code:
```bash
timeout "$REQ_TIMEOUT" mosquitto_sub \
  -h "$MQTT_HOST" -p "$MQTT_PORT" -u "$MQTT_USER" -P "$MQTT_PASS" \
  -N -t "$TOPIC_OUT" -C 1 | xxd -p -c 256 > "$tmp"
```

---

## Step 7: Debug transport contention and verify direct UART path

Why changed:
- Timeouts were intermittently caused by stale/duplicate adapter processes holding the serial device.
- Needed to prove whether firmware actually replies on raw UART independent of MQTT.

Operational actions:
- Cleared adapter/process contention.
- Ran direct serial frame probe against ST-Link VCP.

Validation result:
```text
TX aaaaaaaa000b00016f74
RX chunk aaaaaaaa000b00026f0123
RX total aaaaaaaa000b00026f0123
```

Interpretation:
- Firmware on reverted VCP image responded correctly at protocol level on channel 11.

---

## Step 8: Correct MQTT topic usage during verification

Why changed:
- Some failing probes used wrong topic format (`.../channel/11/...`).
- Current adapter code expects `<root>/<channel_id>/in|out`, e.g. `bthermo/11/in`.

Verification command pattern used:
- subscribe: `bthermo/11/out`
- publish: `bthermo/11/in`

Binary payload verification result:
```text
SUB_RC=0
RESP_BYTES=3
RESP_HEX=6f010a
```

Interpretation:
- MQTT bridge and firmware round-trip are working in current flashed state.

---

## Step 9: Revert temporary RX buffer debug tweak to production-like value

Why changed:
- A temporary diagnostic change reduced RX DMA buffer size to help investigate timing behavior.
- After diagnosis, reverted to normal value for production-like verification.

File changed:
- `components/uart2-channel/core/src/main.rs`

Final code state:
```rust
const RX_BUFFER_SIZE: usize = 128;
static mut RX_BUFFER: [u8; RX_BUFFER_SIZE] = [0xAA; RX_BUFFER_SIZE];
```

Post-revert checks completed:
- reflashed successfully
- raw UART request/response passed
- MQTT binary round-trip passed

---

## Step 10: Fix `clear_programs` so script does not abort under `set -e`

Why changed:
- `clear_programs` previously used `[[ "$r" == "6400" ]] && ...`.
- Under `set -e`, non-`6400` result on the last loop iteration could yield non-zero function status and abort the script unexpectedly.

File changed:
- `test/output_protocol_test.sh`

Code change:
```bash
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
```

Why this works:
- Every response path is handled intentionally.
- Function explicitly returns success, avoiding accidental aborts from the loop tail condition.

---

## Verification summary

Firmware/build:
- VCP image build and flash path completed successfully (`App.vcp.toml` + `flash_output_test_vcp.sh`).

Transport verification:
- Raw UART (ST-Link VCP): confirmed reply frame.
- MQTT bridge: confirmed binary reply (`6f010a`) on `bthermo/11/out` after publishing to `bthermo/11/in`.

Script stability:
- `output_protocol_test.sh` now handles cleanup responses robustly and avoids false `set -e` exits from `clear_programs`.

---

## Files manually changed during this work (functional)

- `app/bthermo/App.vcp.toml` (new)
- `components/uart2-channel/api/*` (new component API)
- `components/uart2-channel/core/*` (new transport component)
- `utils/mqtt_adapter/main.py`
- `utils/mqtt_adapter/settings.yaml`
- `test/flash_output_test_vcp.sh`
- `test/output_protocol_test.sh`

Additional generated/derived files were produced by build/flash/test tooling and are not part of the hand-authored logic changes above.

---

## Repro context

- Date: 2026-04-17
- Repo head at report time: `7c01867`
