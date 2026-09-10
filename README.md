# ConceptOS — Component-Level Delta Updates

> **This is a fork.** ConceptOS itself is by
> [Andrea Aspesi](https://github.com/andreaaspesidev/concept-os), and ConceptOS is
> in turn a fork of [Hubris](https://github.com/oxidecomputer/hubris) by Oxide
> Computer Company. Everything described under "My contribution" below is my work;
> the operating system it builds on is not.

Undergraduate thesis, Department of Computer Science & Engineering, Rajshahi
University of Engineering & Technology.

**Thesis:** *Component-Level Delta Updates in ConceptOS: Extending a Reboot-Free
Component-Update System for Live Firmware Patching*
**Supervisor:** Dr. Md. Nazrul Islam Mondal, Professor, Dept. of CSE, RUET

## The problem

ConceptOS can already replace a single firmware component on a running device
without rebooting it. But it sends the whole component every time, so changing one
byte costs the same to transmit as shipping a new component. On a battery-powered
device over a slow radio, that is the dominant cost of an update.

The obvious fix is to send only the difference between the old and new versions.
The reason that is not straightforward here:

**ConceptOS does not store components as they were compiled.** It keeps
position-dependent code and relocates it at install time by patching addresses
directly into flash. So the bytes on the device differ from the pristine build at
every relocation site. Copying such a byte into a patch would reproduce an
already-relocated address, and the installer would then relocate it a second time
and corrupt the component.

Existing binary-differencing work sidesteps this by *normalising* addresses before
diffing. That option is not available here: the source is the device's own
relocated flash, and the target will be relocated again after installation.

## My contribution

- A **flash-faithful encoding invariant** — instead of normalising addresses, the
  encoder is forbidden from emitting COPY instructions that read from a relocation
  site or the trailer. Those regions go into ADD instead, at a small size cost.
- A **single-write reconstruction** that assembles the new image directly into its
  destination flash block, rather than staging it elsewhere first.
- `libs/delta_patcher` — a `no_std`-compatible encoder and decoder with a custom
  patch format, built end to end and integrated into the ConceptOS component
  delivery protocol.
- Evaluation on real hardware across three transports.

## Results

Measured on an STM32L476RG (NUCLEO-L476RG):

| Metric | Result |
|---|---|
| Bytes on the wire | **67% fewer** |
| Update time | **~60% lower** |
| Unavailability | unchanged |
| Bytes programmed / page erases | unchanged |
| Code size in the update component | ~50% larger |

Correctness held over 2,500 fuzzed round trips.

Two findings worth stating plainly, because they limit how far the result carries:

- The saving comes from **fewer protocol round trips**, not less time on the wire.
  Link utilisation never exceeded 46% at any operating point, confirmed by a
  fragment-size sweep. On a duty-cycled radio the picture would differ.
- Delta reduces *transfer* cost but **not flash wear** — bytes programmed and page
  erases are unchanged.

An experiment intended to show that the flash-faithful constraint explains the
encoder's size gap against `bsdiff` instead refuted that explanation: with only
eight relocation sites in a 5.9 KB image the constraint is nearly free, and the gap
is better attributed to the absence of entropy coding and approximate matching.

Known limitation: the update component is left with a 24-byte stack margin, which
is demonstrably insufficient — doubling the fragment size overflows it.

## Layout

Work specific to this thesis:

```
libs/delta_patcher/
  src/encoder.rs      greedy matcher with the flash-faithful invariant
  src/decoder.rs      patch application, single-write reconstruction
  src/format.rs       patch container format
  tests/roundtrip.rs          encode/apply round trips
  tests/encode_faithful.rs    invariant is never violated
  tests/decoder_handcrafted.rs
  examples/measure.rs         size and timing measurement
  examples/simulate_device.rs host-side device simulation
```

Everything else in the tree is ConceptOS or Hubris.

## Branches

| Branch | Contents |
|---|---|
| `delta` | the delta update implementation — **start here** |
| `delta-performance` | instrumented build used for the measurements |
| `main`, `bthermo`, others | upstream ConceptOS |

## Building

Requires the Rust toolchain pinned in `rust-toolchain.toml` and an ARM
cross-compilation target. See the upstream ConceptOS documentation for board setup
and flashing; the delta work does not change that process.

The patcher library builds and tests on the host on its own:

```bash
cd libs/delta_patcher
cargo test
```

## Licence

Inherits the upstream licence — see [LICENSE](LICENSE). Hubris is MPL-2.0.
