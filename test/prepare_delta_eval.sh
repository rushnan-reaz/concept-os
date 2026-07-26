#!/usr/bin/env bash
# Prepare a clean two-build delta evaluation for `bthermo`:
#
#   v1  = committed bthermo (version 2)                       -> the flashed BASE
#   v2  = bthermo + clamp_reading + version bump (VER2, def 3) -> carried by the DELTA
#
# Produces:
#   /tmp/bthermo_v1.hbf        (byte-identical to what the base system flash installs)
#   /tmp/bthermo_v2.hbf
#   /tmp/bthermo_delta.hbf     (v1 -> v2 delta, self-checked by delta_gen)
#
# The bthermo source is always restored to v1 afterwards, so a subsequent
# `flash_output_test_vcp.sh` (or flash_output_test.sh) always flashes v1.
#
# Env:
#   VER2=3   target version for v2 (must be >= v1's version)
set -euo pipefail

ROOT_DIR="/home/rushnan-reaz/concept-os"
CORE="$ROOT_DIR/components/bthermo/core"
DELTA_GEN="$ROOT_DIR/toolchain/modules/delta_gen/target/debug/delta_gen"
VER2="${VER2:-2}"

cd "$ROOT_DIR"

# Snapshot the exact current working-tree state as "v1" and restore it verbatim
# afterwards (works whether or not the change is committed — honors your edits).
BK="$(mktemp -d)"
cp "$CORE/src/tmp117.rs" "$BK/tmp117.rs"
cp "$CORE/Component.toml" "$BK/Component.toml"
restore() {
  cp "$BK/tmp117.rs" "$CORE/src/tmp117.rs"
  cp "$BK/Component.toml" "$CORE/Component.toml"
  rm -rf "$BK"
}
trap restore EXIT

# --- Build v1 (committed baseline) -----------------------------------------
echo "[1/4] Building v1 (baseline, version $(grep -m1 '^version' "$CORE/Component.toml" | grep -o '[0-9]*'))"
( cd "$CORE" && make build-l476rg >/dev/null )
cp "$CORE/bthermo.hbf" /tmp/bthermo_v1.hbf
echo "      -> /tmp/bthermo_v1.hbf ($(stat -c%s /tmp/bthermo_v1.hbf) B)"

# --- Build v2 (clamp_reading + version bump) -------------------------------
echo "[2/4] Building v2 (clamp_reading, version $VER2)"
python3 - "$CORE" "$VER2" <<'PY'
import sys
core, ver2 = sys.argv[1], sys.argv[2]
# tmp117.rs: add clamp_reading and route the reading through it
p = f"{core}/src/tmp117.rs"
s = open(p).read()
if "clamp_reading" not in s:
    s = s.replace(
        "const UPDATE_MS: u64 = 1000;",
        "const UPDATE_MS: u64 = 1000;\n\n"
        "/// Clamp a reading to the TMP117's rated range (v2 change).\n"
        "fn clamp_reading(t: f32) -> f32 {\n"
        "    if t < -55.0 { -55.0 } else if t > 150.0 { 150.0 } else { t }\n"
        "}",
    )
    s = s.replace(
        "self.last_temp = ((raw_temp >> 4) as f32) * TMP117_RESOLUTION;",
        "self.last_temp = clamp_reading(((raw_temp >> 4) as f32) * TMP117_RESOLUTION);",
    )
    open(p, "w").write(s)
# Component.toml: bump version
p = f"{core}/Component.toml"
lines = open(p).read().splitlines(keepends=True)
for i, ln in enumerate(lines):
    if ln.lstrip().startswith("version"):
        lines[i] = f"version = {ver2}\n"
        break
open(p, "w").write("".join(lines))
print("  applied clamp_reading + version =", ver2)
PY
( cd "$CORE" && make build-l476rg >/dev/null )
cp "$CORE/bthermo.hbf" /tmp/bthermo_v2.hbf
echo "      -> /tmp/bthermo_v2.hbf ($(stat -c%s /tmp/bthermo_v2.hbf) B)"

# --- Restore v1 source + rebuild committed hbf ------------------------------
echo "[3/4] Restoring bthermo source to v1"
restore
trap - EXIT
( cd "$CORE" && make build-l476rg >/dev/null )   # bthermo.hbf back to v1 on disk

# --- Generate the delta -----------------------------------------------------
echo "[4/4] Generating delta v1 -> v2"
[ -x "$DELTA_GEN" ] || ( cd "$ROOT_DIR/toolchain/modules/delta_gen" && cargo build >/dev/null )
"$DELTA_GEN" /tmp/bthermo_v1.hbf /tmp/bthermo_v2.hbf -o /tmp/bthermo_delta.hbf

cat <<EOF

Ready.
  BASE  (flash this): source is at v1; run  test/flash_output_test_vcp.sh
  DELTA (push this):  /tmp/bthermo_delta.hbf   (v1 -> v2, version $VER2)

Typical eval loop:
  ./flash_output_test_vcp.sh                       # device <- v1
  ./delta_update_test.sh --delta /tmp/bthermo_delta.hbf   # v1 -> v2 over the air
  # 'info' after should show component id=10 version=$VER2
EOF
