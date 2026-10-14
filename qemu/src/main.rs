//! The ota-safe bootloader decision, on a Cortex-M3.
//!
//! QEMU loads a flash image at `FLASH_BASE` (`-device loader`). This program
//! treats that memory as the boot flash, runs `ota_safe::boot` on it exactly
//! as a bootloader would, and prints the decision and the metadata record
//! the bootloader left over semihosting, then exits. `make qemu` compares
//! every line with what the host simulator decided for the same image.
#![no_std]
#![no_main]

use core::fmt::Write;
use cortex_m_rt::entry;
use cortex_m_semihosting::{debug, hio};
use ed25519_dalek::VerifyingKey;
use ota_safe::{Boot, Config, Flash, Layout};

const FLASH_BASE: usize = 0x2030_0000;

/// The simulator's public key (`ota-sim key`); scripts/qemu-check.sh checks it.
const PUBLIC_KEY: [u8; 32] = [
    0xea, 0x4a, 0x6c, 0x63, 0xe2, 0x9c, 0x52, 0x0a, 0xbe, 0xf5, 0x50, 0x7b, 0x13, 0x2e, 0xc5, 0xf9,
    0x95, 0x47, 0x76, 0xae, 0xbe, 0xbe, 0x7b, 0x92, 0x42, 0x1e, 0xea, 0x69, 0x14, 0x46, 0xd2, 0x2c,
];

/// RAM standing in for NOR flash: programming can only clear bits.
struct RamFlash {
    size: u32,
    sector: u32,
}

impl Flash for RamFlash {
    type Error = ();
    fn read(&mut self, addr: u32, buf: &mut [u8]) -> Result<(), ()> {
        for (i, b) in buf.iter_mut().enumerate() {
            *b = peek(addr + i as u32);
        }
        Ok(())
    }
    fn erase_sector(&mut self, addr: u32) -> Result<(), ()> {
        assert!(addr.is_multiple_of(self.sector) && addr < self.size);
        for a in addr..addr + self.sector {
            poke(a, 0xFF);
        }
        Ok(())
    }
    fn program(&mut self, addr: u32, data: &[u8]) -> Result<(), ()> {
        for (i, d) in data.iter().enumerate() {
            let a = addr + i as u32;
            poke(a, peek(a) & d);
        }
        Ok(())
    }
}

fn peek(addr: u32) -> u8 {
    // SAFETY: FLASH_BASE..+size is RAM that QEMU loaded the image into and
    // nothing else in this program uses.
    unsafe { core::ptr::read_volatile((FLASH_BASE + addr as usize) as *const u8) }
}

fn poke(addr: u32, v: u8) {
    // SAFETY: as for `peek`.
    unsafe { core::ptr::write_volatile((FLASH_BASE + addr as usize) as *mut u8, v) }
}

#[entry]
fn main() -> ! {
    let mut out = hio::hstdout().unwrap();
    let layout = Layout::standard();
    let mut flash = RamFlash {
        size: layout.size(),
        sector: layout.sector,
    };
    let key = VerifyingKey::from_bytes(&PUBLIC_KEY).unwrap();
    let cfg = Config::default();
    let decision = ota_safe::boot(&mut flash, &layout, &key, &cfg).unwrap();
    match decision {
        Boot::Slot {
            slot,
            version,
            trial,
        } => {
            let _ = write!(out, "boot slot={slot:?} version={version} trial={trial}");
        }
        Boot::Brick(b) => {
            let _ = write!(out, "brick {b:?}");
        }
    }
    match ota_safe::read_meta(&mut flash, &layout, &cfg).unwrap() {
        Some((m, _)) => {
            let _ = writeln!(out, " meta={m:?}");
        }
        None => {
            let _ = writeln!(out, " meta=None");
        }
    }
    debug::exit(debug::EXIT_SUCCESS);
    loop {
        core::hint::spin_loop();
    }
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    if let Ok(mut out) = hio::hstdout() {
        let _ = writeln!(out, "panic: {info}");
    }
    debug::exit(debug::EXIT_FAILURE);
    loop {
        core::hint::spin_loop();
    }
}
