#!/usr/bin/env bash
# Regenerate the 6 churn-variant deltas fresh against TODAY's actual v1
# baseline (the old host-generated deltas from earlier in this session were
# built against a different link-time layout and fail DeltaBaseCrcMismatch
# on real hardware). Rebuilds each variant from its saved source snapshot in
# test/churn_variants/sources/, regenerates the delta via delta_gen.
set -euo pipefail

ROOT="/home/rushnan-reaz/concept-os"
CORE="$ROOT/components/bthermo/core"
MAIN_RS="$CORE/src/main.rs"
TMP102="$CORE/src/tmp102.rs"
CFG="$CORE/Component.toml"
DELTA_GEN="$ROOT/toolchain/modules/delta_gen/target/release/delta_gen"
SRC_DIR="$ROOT/test/churn_variants/sources"
OUT_DIR="$ROOT/test/churn_variants/deltas"
HBF_DIR="$ROOT/test/churn_variants/hbf"

mkdir -p "$OUT_DIR" "$HBF_DIR"

BK="$(mktemp -d)"
cp "$TMP102" "$BK/tmp102.rs"
cp "$MAIN_RS" "$BK/main.rs"
cp "$CFG" "$BK/Component.toml"
restore() {
  cp "$BK/tmp102.rs" "$TMP102"
  cp "$BK/main.rs" "$MAIN_RS"
  cp "$BK/Component.toml" "$CFG"
  rm -rf "$BK"
}
trap restore EXIT

echo "[0/N] Building today's real v1 baseline"
( cd "$CORE" && make build-l476rg >/dev/null )
cp "$CORE/bthermo.hbf" "$HBF_DIR/v1_baseline.hbf"
echo "      -> $HBF_DIR/v1_baseline.hbf ($(stat -c%s "$HBF_DIR/v1_baseline.hbf") B)"

VARIANTS="in_place_tiny in_place_medium in_place_large size_change_small size_change_medium size_change_large"

for variant in $VARIANTS; do
  echo "=== $variant ==="
  cp "$SRC_DIR/tmp102_${variant}.rs" "$TMP102"

  if [[ "$variant" == size_change_* ]]; then
    sed -i 's/thermo\.init_hardware(i2c)?;/thermo.init_hardware(i2c, rcc)?;/' "$MAIN_RS"
    # LED2 driver touches GPIOA directly -- needs the peripheral grant or it
    # MPU-faults on the device the moment init_hardware() runs.
    sed -i "s/^peripherals = \['gpiob','gpioc','i2c3'\]/peripherals = ['gpioa','gpiob','gpioc','i2c3']/" "$CFG"
  fi

  # bump version to 2
  sed -i 's/^version = .*/version = 2/' "$CFG"

  ( cd "$CORE" && make build-l476rg >/dev/null )
  cp "$CORE/bthermo.hbf" "$HBF_DIR/${variant}.hbf"
  echo "  built -> $HBF_DIR/${variant}.hbf ($(stat -c%s "$HBF_DIR/${variant}.hbf") B)"

  "$DELTA_GEN" "$HBF_DIR/v1_baseline.hbf" "$HBF_DIR/${variant}.hbf" -o "$OUT_DIR/${variant}.delta.hbf"
  echo "  delta -> $OUT_DIR/${variant}.delta.hbf ($(stat -c%s "$OUT_DIR/${variant}.delta.hbf") B)"

  # restore for next iteration
  cp "$BK/tmp102.rs" "$TMP102"
  cp "$BK/main.rs" "$MAIN_RS"
  cp "$BK/Component.toml" "$CFG"
done

echo
echo "[final] Restoring bthermo source to v1 and rebuilding committed hbf"
( cd "$CORE" && make build-l476rg >/dev/null )

echo "Done. Deltas in $OUT_DIR, base in $HBF_DIR/v1_baseline.hbf"
