# ota-safe

[![CI](https://github.com/ahmed-hashim-pro/ota-safe/actions/workflows/ci.yml/badge.svg)](https://github.com/ahmed-hashim-pro/ota-safe/actions/workflows/ci.yml) [![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

A/B firmware updates for microcontrollers, with a trial boot and automatic
rollback, in `no_std` Rust with no allocation. The repository also contains
the tool that checks the design: a NOR flash simulator that cuts power at
every erase and every program of a device's life, in every way that
operation can be left half-done, and then checks that the device still
boots.

A firmware update is the riskiest thing a deployed device does. It happens
in the field, often on a battery, and when power goes halfway through, a
device that can't boot needs someone to come and reflash it. Update schemes
are usually checked by pulling the plug a few times at whatever moment the
tester happens to choose. A device's update path is a few hundred flash
operations, and the dangerous moments are usually one or two of them. This
repository cuts power at all of them:

![Three strips of coloured columns, one column per flash operation of a device going from v1 to v2 to v3. Naive in-place: almost entirely red, 437 of 443 cuts brick the device. Naive A/B: green with two thin red bands where the active-slot flag is erased and rewritten, 14 of 459 cuts brick it. ota-safe: entirely green, 0 of 491 cuts fail. Labels under each strip mark where v2 and v3 are written.](docs/cuts.svg)

```
$ ota-sim matrix
good: v1 installs v2, v2 installs v3; every version healthy
scheme             no cut flash ops       cuts       ok  bricked    stuck   unsafe
naive in-place         ok        91        443        6      437        0        0   98.6% bricked
naive A/B              ok        95        459      445       14        0        0   3.1% bricked
ota-safe               ok       103        491      491        0        0        0   every cut ok

bad-update: the same, but v3 crashes before it can confirm itself
scheme             no cut flash ops       cuts       ok  bricked    stuck   unsafe
naive in-place      STUCK        91        443        0      437        6        0   98.6% bricked, 1.4% stuck
naive A/B           STUCK        95        459        0       14      445        0   3.1% bricked, 96.9% stuck
ota-safe               ok       107        507      507        0        0        0   every cut ok
```

- **Naive in-place** rewrites the only slot. Power lost anywhere between the
  first erase and the last page leaves nothing valid to boot: 98.6% of cuts.
- **Naive A/B** writes the new image into the other slot, then erases and
  rewrites a one-byte "active slot" flag. Writing the image is safe. The
  14 cuts that brick it all fall in the two flag rewrites, so pulling the
  plug at a random moment finds one about 3% of the time. With a bad update
  it has no way back: v3 crashes on every boot, forever.
- **ota-safe** survives every one of the 998 single cuts. It also survives
  all 356,028 lives at depth 2, where a second cut lands after the device
  comes back (`make deep`).
  When v3 never confirms itself, it gets three boots, then the device rolls
  back to v2 and refuses v3 from then on.

![Terminal recording: ota-sim matrix prints the two tables above; a naive A/B device has power cut at op 44, while its active-slot flag is being erased, and the next boot bricks with "active-slot flag is neither A nor B"; an ota-safe device cut at op 44 boots v1 on the next power-on, installs v2 again, and goes on to confirm v3; ota-sim mutants shows all six planted bugs caught](docs/demo.gif)

The same bootloader code, built for a Cortex-M3 and run in QEMU, makes the
same decision as the simulator on all 998 cut states
([below](#on-a-cortex-m3)).

## Quickstart

You need Rust 1.81 or newer.

```bash
git clone https://github.com/ahmed-hashim-pro/ota-safe.git && cd ota-safe
cargo build --release
export PATH=$PWD/target/release:$PATH

ota-sim matrix                                   # the tables above, under a second
ota-sim life --scheme naive-ab --cut 44:erase-0  # one cut, event by event
ota-sim life --cut 44:erase-0                    # the same cut, ota-safe
ota-sim mutants                                  # six planted bugs, all caught
cargo test                                       # 33 tests, about 20 s
```

`make deep` runs the second cut at every operation after the first: 356,028
lives for ota-safe, plus the naive schemes, in about two and a half minutes
on 12 cores, then the same check as a test. `make qemu` runs the
Cortex-M3 check below; it needs `rustup target add thumbv7m-none-eabi` and
`qemu-system-arm`. With nothing installed but Docker, `make docker-check`
builds an image with all of it and runs lint, tests, the docs check and the
QEMU check.

## How it works

The flash holds two metadata sectors and two image slots. In the standard
layout that is 4 KiB sectors, 256-byte pages and two 16 KiB slots: 40 KiB.

```
0x00000  metadata sector 0     one 32-byte record
0x01000  metadata sector 1     one 32-byte record
0x02000  slot A                header page, then the image body
0x06000  slot B                header page, then the image body
```

**An image** is a 256-byte header followed by the body. The header holds a
magic number, the version, the body length, the SHA-256 of the body, and an
Ed25519 signature over those 44 bytes. The bootloader checks the signature
and the hash before every boot.

**A metadata record** says which slot is confirmed, and at which version;
whether a trial is pending, in which slot, for which version and with how
many boots left; and the last version that was rolled back. It also carries
a sequence number and a CRC-32. The current record is the valid one with the
highest sequence number. Every change writes a new record into the sector
that does *not* hold the current one: erase it, program 32 bytes. If power
goes during either step, the current record is still there and still wins.

**An update** (`ota_safe::update`) refuses images that are unsigned, not
newer than the confirmed version, or the version that was last rolled back.
It erases the slot that is not confirmed, programs the body and then the
header page, reads the slot back and verifies it, and only then writes a
record that starts the trial with three boots. The confirmed slot is never
erased or written.

**The bootloader** (`ota_safe::boot`) reads the current record. If a trial
is pending, has boots left, and its slot verifies as that version, it writes
a record with one boot fewer *before* jumping to it. Otherwise it writes a
record that ends the trial and marks the version rejected, then boots the
confirmed slot. If the confirmed slot doesn't verify, or no record is valid,
it reports a brick rather than guessing.

**The firmware** calls `ota_safe::confirm` once it is up and healthy. If it
is the trial, that one record write makes it the confirmed image.

Each step's only commit point is a single record write, and the record that
was current before it stays valid until the new one is complete. That is
what the search below checks.

## The flash model and the search

`SimFlash` models NOR flash. Erasing sets a 4 KiB sector to `0xFF`.
Programming stays within one 256-byte page and can only clear bits, so
programming over data that isn't erased ANDs the two. Out-of-range or
page-crossing writes are bugs and panic.

A power cut is armed at the *n*-th erase or program after power-on. When it
fires, the operation leaves one of these, and every later operation fails
until power returns:

| interrupted operation | what is left |
|---|---|
| any | `before`: nothing done (also covers "after the previous one finished") |
| program | `prefix-1`, `prefix-2`, `prefix-3`: the first quarter, half or three quarters of the bytes programmed, the next byte only partly |
| program | `scatter`: each bit that should clear did or didn't, at random |
| erase | `erase-0`, `erase-1`: each byte either erased or with only some bits set back to 1, two different patterns |

A device's **life** is the same in every scheme. v1 installs v2 and reboots.
v2 confirms itself, installs v3 and reboots. v3 confirms itself and settles,
or, in the bad-update workload, crashes before confirming every time it
starts. After a power cut, the device powers on and carries on from wherever
its flash says it is.

After every boot the checker reads the flash itself. It doesn't trust the
bootloader's report. It asks three things:

- does the booted slot hold a valid, signed image of the version booted?
- is that version at least the newest one that has already been confirmed
  (for the naive schemes, which have no trial: committed)?
- has an unconfirmed version booted more often than the trial limit allows?

At the end the device must have settled: on v3, or on v2 with v3 rolled back
in the bad-update workload. A life that bricks, boots something it must not,
or doesn't settle within 16 boots fails.

The search runs one life per cut: every operation of the uncut life, every
tear that operation allows. That is 491 lives for the good workload and 507
for the bad one. At depth 2 each of those lives gets a second cut at every
operation after it powers back on, in every way:

```
$ ota-sim matrix --depth 2
good: v1 installs v2, v2 installs v3; every version healthy
scheme             no cut flash ops   1-2 cuts       ok  bricked    stuck   unsafe
naive in-place         ok        91       1573       21     1552        0        0   98.7% bricked
naive A/B              ok        95     153654   149020     4634        0        0   3.0% bricked
ota-safe               ok       103     174014   174014        0        0        0   every cut ok

bad-update: the same, but v3 crashes before it can confirm itself
scheme             no cut flash ops   1-2 cuts       ok  bricked    stuck   unsafe
naive in-place      STUCK        91       1573        0     1552       21        0   98.7% bricked, 1.3% stuck
naive A/B           STUCK        95     153654        0     4634   149020        0   3.0% bricked, 97.0% stuck
ota-safe               ok       107     182014   182014        0        0        0   every cut ok
```

Every life is a pure function of its cuts, so any line of any table replays
exactly: `ota-sim life --scheme NAME --workload W --cut AT:TEAR [--cut AT:TEAR]`.
`ota-sim dump --cut AT:TEAR -o flash.bin` writes the flash as the cut left
it.

## Planted bugs

A search that never fails proves nothing about the search. ota-safe has six
deliberate bugs behind a `mutants` feature, which firmware never enables,
and the same search runs against each:

```
$ ota-sim mutants
planted bug              result       no cut  failing cuts   first failure
none (ota-safe)          clean            ok         0/998   -
single-meta-copy         caught           ok        98/998   good 44:erase-0: BRICKED: no valid metadata record
no-crc                   caught           ok         3/998   bad-update 102:prefix-2: UNSAFE: unconfirmed v3 booted 4 times (limit 3)
ignore-seq               caught        STUCK     7104/7104   good no cut: STUCK: still rebooting after 16 boots
decrement-after-boot     caught       UNSAFE       467/942   bad-update no cut: UNSAFE: unconfirmed v3 booted 4 times (limit 3)
rollback-without-reject  caught       UNSAFE      752/1243   bad-update no cut: UNSAFE: unconfirmed v3 booted 4 times (limit 3)
trial-before-image       caught           ok       651/998   good 2:before: STUCK: settled on v1, expected v3
```

| bug | what it changes |
|---|---|
| `single-meta-copy` | every record is written into sector 0, erased in place |
| `no-crc` | a record's CRC isn't checked |
| `ignore-seq` | the first valid record wins, not the one with the highest sequence number |
| `decrement-after-boot` | the trial boot is counted by the application after it starts, not by the bootloader before |
| `rollback-without-reject` | a rolled-back version isn't recorded as rejected |
| `trial-before-image` | the updater starts the trial, then writes the image |

Three of these (`ignore-seq`, `decrement-after-boot`,
`rollback-without-reject`) are logic bugs that an ordinary test of a bad
update finds with the power on. The other three pass every test with the
power on, and that's the reason for the search:

- `single-meta-copy` bricks when power goes between erasing the only
  metadata sector and writing the new record.
- `trial-before-image` is cut while the image is being written. The
  bootloader then finds a trial slot that doesn't verify. It rolls back
  correctly, but it marks v2 rejected, so the device never installs v2 at
  all.
- `no-crc` fails on 3 of 998 cuts. Power goes halfway through a v3 trial
  boot's record, so the record has a valid magic number, sequence number and
  slots, but its trial and rejected version fields are still mostly erased.
  Without the CRC it is believed. The half-written trial version doesn't
  match slot A, so the bootloader ends the trial and records that garbage
  version as rejected, not v3. v2 then installs v3 again, and v3 gets three fresh boots on top of
  the ones it already had. A test campaign that pulls the plug at random
  would need luck to find this.

## On a Cortex-M3

The core crate is `no_std`, so the bootloader decision can run where a
bootloader runs. `qemu/` builds `ota_safe::boot` for `thumbv7m-none-eabi`
and runs it on QEMU's `mps2-an385` board (a Cortex-M3). `scripts/qemu-check.sh`
writes every depth-1 cut state of ota-safe as a 40 KiB flash image, with
the host simulator's decision for each. It loads each image into the
emulated board's memory, lets the Cortex-M3 decide what to boot (including
any metadata write it makes first), and compares the decision and the
resulting record over semihosting:

```
$ make qemu
...
998 flash images: the Cortex-M3 build decided as the host did on 998 of 998
```

This shows that the same source, compiled for a 32-bit ARM core with no
standard library, behaves identically. On the board, "flash" is RAM with the
same AND-only programming rule. No real flash controller and no real power
cut are involved. The QEMU program is 55 KB of code at `opt-level = "s"`,
most of it Ed25519 and the formatting for its output.

## Using it in firmware

Implement `Flash` for your part's driver, then call three functions:

```rust
use ota_safe::{Boot, Config, Flash, Layout};

struct Nor { /* your driver */ }

impl Flash for Nor {
    type Error = DriverError;
    fn read(&mut self, addr: u32, buf: &mut [u8]) -> Result<(), DriverError> { todo!() }
    fn erase_sector(&mut self, addr: u32) -> Result<(), DriverError> { todo!() }
    fn program(&mut self, addr: u32, data: &[u8]) -> Result<(), DriverError> { todo!() }
}

// Bootloader: decide, then jump.
match ota_safe::boot(&mut nor, &LAYOUT, &KEY, &Config::default())? {
    Boot::Slot { slot, .. } => jump_to(LAYOUT.slots[slot.index()] + ota_safe::HEADER_LEN),
    Boot::Brick(_) => recovery_mode(),
}

// Application, once it is healthy:
ota_safe::confirm(&mut nor, &LAYOUT, &Config::default(), running_slot)?;

// Application, with a downloaded image in memory:
ota_safe::update(&mut nor, &LAYOUT, &KEY, &Config::default(), &image)?;
```

This snippet shows the shape of the API; it isn't compiled as part of the
build. The signing side is `ota_safe::unsigned_header` plus an Ed25519
signature over its first 44 bytes. `sim/src/scheme.rs` (`make_image`) does
exactly that.

## What this does not do

- **It has not run on hardware.** The flash is a model of NOR behaviour,
  and the Cortex-M3 runs in QEMU with RAM standing in for flash. Real parts
  have their own failure modes: a sector can read back fine and then lose
  bits after a marginal erase, and power can fail slowly with brown-out
  behaviour. None of that is modelled.
- **NAND is not modelled**: no bad blocks, no ECC, no
  no-partial-page-reprogram rule.
- **The search is exhaustive only for this life and these tears.** It covers
  one or two cuts at every operation of a v1 → v2 → v3 life with the tear
  patterns in the table above. It doesn't cover three or more cuts, other
  update orders, or other tear shapes.
- **No secure boot.** The signature check stops an unsigned image, but
  nothing protects the bootloader, the key or the metadata from someone who
  can write the flash directly. Rollback protection is "never older than the
  confirmed version, never the last rejected version". There is no monotonic
  hardware counter.
- **The image must be in RAM to update.** `update` takes the whole image as
  a slice; there is no streaming writer for downloads larger than RAM.
- **Images must run from their slot.** The bootloader picks a slot and
  doesn't copy or swap images, so each image must be linked for the slot it
  lands in or be position-independent.
- **Wear.** Every trial boot and every state change erases a metadata
  sector. That's a handful of erases per update, well within typical NOR
  endurance, and the trial limit caps what a crashing trial can add.
- **Boot time isn't measured.** Every boot hashes and verifies the whole
  image, and on a slow MCU that cost is real.
- **Not published to crates.io.**

## Repository

```
core/      ota-safe: the no_std library (layout, records, verify, boot, update, confirm)
sim/       ota-sim: the flash model, the three schemes, device lives, the search, the chart
  tests/   protocol (power on), search (power cuts), cli
qemu/      the bootloader decision for a Cortex-M3, run in QEMU
scripts/   qemu-check.sh
docker/    an image with Rust, the Cortex-M3 target and QEMU
docs/      cuts.svg, matrix.txt, mutants.txt (CI checks they are current), demo.gif
```

## License

MIT
