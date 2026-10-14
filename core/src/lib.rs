//! A/B firmware updates with a trial boot and automatic rollback, written so
//! that a power cut at any flash operation leaves a device that boots.
//!
//! The flash holds two metadata sectors and two image slots. One slot holds
//! the confirmed image; an update goes into the other one, and becomes a
//! *trial*: the bootloader boots it at most [`Config::max_attempts`] times,
//! spending an attempt *before* each boot. If the new firmware calls
//! [`confirm`] it becomes the confirmed image; if it never does, or if the
//! trial slot fails verification, the bootloader rolls back to the confirmed
//! slot and records the version as rejected so it is not installed again.
//!
//! Every write that changes what the device will boot is a single metadata
//! record, written with a sequence number and a CRC into whichever metadata
//! sector does *not* hold the current record. A power cut during that write
//! leaves the current record intact, so the device boots what it booted
//! before. Images are SHA-256 hashed and Ed25519 signed, and the bootloader
//! verifies a slot before every boot.
//!
//! The crate is `no_std` and allocation-free. It talks to flash through the
//! [`Flash`] trait: read, erase a sector, program within one page.
#![no_std]
#![forbid(unsafe_code)]

use ed25519_dalek::{Signature, VerifyingKey};
use sha2::{Digest, Sha256};

/// Bytes in an image header. The header is the first page of a slot.
pub const HEADER_LEN: u32 = 256;
/// Bytes of the header covered by the signature.
pub const SIGNED_LEN: usize = 44;
/// Bytes in an encoded metadata record.
pub const META_LEN: usize = 32;

const IMAGE_MAGIC: [u8; 4] = *b"OTAI";
const META_MAGIC: [u8; 4] = *b"OTAM";
const NO_TRIAL: u8 = 0xFF;

/// The flash a device boots from, as the bootloader and updater see it.
///
/// `program` may only clear bits (1 to 0), must not cross a page boundary,
/// and bits can only be set again by erasing the whole sector. Any operation
/// may fail part-way through, which is what a power cut looks like.
pub trait Flash {
    type Error;
    fn read(&mut self, addr: u32, buf: &mut [u8]) -> Result<(), Self::Error>;
    fn erase_sector(&mut self, addr: u32) -> Result<(), Self::Error>;
    fn program(&mut self, addr: u32, data: &[u8]) -> Result<(), Self::Error>;
}

/// Where things live in flash. Addresses are offsets from the start of the
/// flash device; sizes must be multiples of the sector size.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    pub sector: u32,
    pub page: u32,
    pub meta: [u32; 2],
    pub slots: [u32; 2],
    pub slot_size: u32,
}

impl Layout {
    /// 4 KiB sectors, 256-byte pages, two 16 KiB slots: 40 KiB in all.
    pub const fn standard() -> Layout {
        Layout {
            sector: 4096,
            page: 256,
            meta: [0, 4096],
            slots: [8192, 8192 + 16384],
            slot_size: 16384,
        }
    }

    /// Total flash the layout uses.
    pub fn size(&self) -> u32 {
        let mut end = 0;
        for a in self.meta {
            end = end.max(a + self.sector);
        }
        for a in self.slots {
            end = end.max(a + self.slot_size);
        }
        end
    }

    /// The largest image body a slot can hold.
    pub fn max_body(&self) -> u32 {
        self.slot_size - HEADER_LEN
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Slot {
    A,
    B,
}

impl Slot {
    pub fn other(self) -> Slot {
        match self {
            Slot::A => Slot::B,
            Slot::B => Slot::A,
        }
    }
    pub fn index(self) -> usize {
        match self {
            Slot::A => 0,
            Slot::B => 1,
        }
    }
    fn from_byte(b: u8) -> Option<Slot> {
        match b {
            0 => Some(Slot::A),
            1 => Some(Slot::B),
            _ => None,
        }
    }
}

/// Planted bugs. With the `mutants` feature, [`Config::mutant`] switches one
/// on, so the power-cut search can show it finds them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Mutant {
    #[default]
    None,
    /// Metadata always goes into sector 0, erased in place.
    SingleMetaCopy,
    /// The CRC of a metadata record is not checked.
    NoCrc,
    /// The first valid record wins, not the one with the highest sequence.
    IgnoreSeq,
    /// The trial attempt is spent by the application after it starts, not
    /// by the bootloader before it boots.
    DecrementAfterBoot,
    /// A rolled-back version is not recorded as rejected.
    RollbackWithoutReject,
    /// The updater marks the trial before writing the image.
    TrialBeforeImage,
}

#[derive(Clone, Copy, Debug)]
pub struct Config {
    /// Boots a trial image gets before the bootloader rolls back.
    pub max_attempts: u8,
    #[cfg(feature = "mutants")]
    pub mutant: Mutant,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            max_attempts: 3,
            #[cfg(feature = "mutants")]
            mutant: Mutant::None,
        }
    }
}

impl Config {
    #[inline]
    fn has(&self, m: Mutant) -> bool {
        #[cfg(feature = "mutants")]
        {
            self.mutant == m
        }
        #[cfg(not(feature = "mutants"))]
        {
            let _ = m;
            false
        }
    }
}

/// A trial: an image that has been written and verified but not confirmed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Trial {
    pub slot: Slot,
    pub version: u32,
    pub attempts_left: u8,
}

/// What the device should boot. Exactly one valid record decides it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Meta {
    pub seq: u32,
    pub confirmed: Slot,
    pub confirmed_version: u32,
    pub trial: Option<Trial>,
    /// The last version that was rolled back; the updater refuses it.
    pub rejected_version: u32,
}

impl Meta {
    pub fn encode(&self) -> [u8; META_LEN] {
        let mut b = [0u8; META_LEN];
        b[0..4].copy_from_slice(&META_MAGIC);
        b[4..8].copy_from_slice(&self.seq.to_le_bytes());
        b[8] = self.confirmed.index() as u8;
        let (ts, tv, ta) = match self.trial {
            Some(t) => (t.slot.index() as u8, t.version, t.attempts_left),
            None => (NO_TRIAL, 0, 0),
        };
        b[9] = ts;
        b[10] = ta;
        b[11] = 0;
        b[12..16].copy_from_slice(&self.confirmed_version.to_le_bytes());
        b[16..20].copy_from_slice(&tv.to_le_bytes());
        b[20..24].copy_from_slice(&self.rejected_version.to_le_bytes());
        b[24..28].copy_from_slice(&[0; 4]);
        let crc = crc32(&b[..28]);
        b[28..32].copy_from_slice(&crc.to_le_bytes());
        b
    }

    /// Decodes a record, or `None` if it is not a valid one.
    pub fn decode(b: &[u8; META_LEN]) -> Option<Meta> {
        Self::decode_with(b, true)
    }

    fn decode_with(b: &[u8; META_LEN], check_crc: bool) -> Option<Meta> {
        if b[0..4] != META_MAGIC {
            return None;
        }
        if check_crc && crc32(&b[..28]) != u32::from_le_bytes([b[28], b[29], b[30], b[31]]) {
            return None;
        }
        let u = |i: usize| u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]);
        let confirmed = Slot::from_byte(b[8])?;
        let trial = match b[9] {
            NO_TRIAL => None,
            s => Some(Trial {
                slot: Slot::from_byte(s)?,
                version: u(16),
                attempts_left: b[10],
            }),
        };
        Some(Meta {
            seq: u(4),
            confirmed,
            confirmed_version: u(12),
            trial,
            rejected_version: u(20),
        })
    }
}

/// CRC-32 (IEEE 802.3), bit by bit: 28 bytes per record do not need a table.
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in data {
        crc ^= byte as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

/// Reads the current metadata record and which sector holds it.
pub fn read_meta<F: Flash>(
    flash: &mut F,
    layout: &Layout,
    cfg: &Config,
) -> Result<Option<(Meta, usize)>, F::Error> {
    let mut best: Option<(Meta, usize)> = None;
    for (i, &addr) in layout.meta.iter().enumerate() {
        let mut b = [0u8; META_LEN];
        flash.read(addr, &mut b)?;
        let Some(m) = Meta::decode_with(&b, !cfg.has(Mutant::NoCrc)) else {
            continue;
        };
        if cfg.has(Mutant::IgnoreSeq) {
            return Ok(Some((m, i)));
        }
        if best.map_or(true, |(b, _)| m.seq > b.seq) {
            best = Some((m, i));
        }
    }
    Ok(best)
}

/// Writes `next` as the new current record, with the next sequence number,
/// into the sector that does not hold `current`.
fn write_meta<F: Flash>(
    flash: &mut F,
    layout: &Layout,
    cfg: &Config,
    current: (Meta, usize),
    mut next: Meta,
) -> Result<(Meta, usize), F::Error> {
    next.seq = current.0.seq.wrapping_add(1);
    let target = if cfg.has(Mutant::SingleMetaCopy) {
        0
    } else {
        1 - current.1
    };
    let addr = layout.meta[target];
    flash.erase_sector(addr)?;
    flash.program(addr, &next.encode())?;
    Ok((next, target))
}

/// What a valid image says about itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImageInfo {
    pub version: u32,
    pub body_len: u32,
}

fn check_header(
    header: &[u8],
    layout: &Layout,
    key: &VerifyingKey,
) -> Option<(ImageInfo, [u8; 32])> {
    if header[0..4] != IMAGE_MAGIC {
        return None;
    }
    let u = |i: usize| u32::from_le_bytes([header[i], header[i + 1], header[i + 2], header[i + 3]]);
    let info = ImageInfo {
        version: u(4),
        body_len: u(8),
    };
    if info.body_len > layout.max_body() {
        return None;
    }
    let mut sig = [0u8; 64];
    sig.copy_from_slice(&header[SIGNED_LEN..SIGNED_LEN + 64]);
    key.verify_strict(&header[..SIGNED_LEN], &Signature::from_bytes(&sig))
        .ok()?;
    let mut digest = [0u8; 32];
    digest.copy_from_slice(&header[12..44]);
    Some((info, digest))
}

/// Checks an image held in memory: header, signature and body hash.
pub fn verify_image(image: &[u8], layout: &Layout, key: &VerifyingKey) -> Option<ImageInfo> {
    if image.len() < HEADER_LEN as usize {
        return None;
    }
    let (info, digest) = check_header(&image[..HEADER_LEN as usize], layout, key)?;
    if image.len() != (HEADER_LEN + info.body_len) as usize {
        return None;
    }
    let got: [u8; 32] = Sha256::digest(&image[HEADER_LEN as usize..]).into();
    (got == digest).then_some(info)
}

/// Checks the image in a slot, reading it a page at a time.
pub fn verify_slot<F: Flash>(
    flash: &mut F,
    layout: &Layout,
    slot: Slot,
    key: &VerifyingKey,
) -> Result<Option<ImageInfo>, F::Error> {
    let base = layout.slots[slot.index()];
    let mut header = [0u8; HEADER_LEN as usize];
    flash.read(base, &mut header)?;
    let Some((info, digest)) = check_header(&header, layout, key) else {
        return Ok(None);
    };
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 256];
    let mut off = 0;
    while off < info.body_len {
        let n = (info.body_len - off).min(buf.len() as u32) as usize;
        flash.read(base + HEADER_LEN + off, &mut buf[..n])?;
        hasher.update(&buf[..n]);
        off += n as u32;
    }
    let got: [u8; 32] = hasher.finalize().into();
    Ok((got == digest).then_some(info))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Brick {
    /// Neither metadata sector holds a valid record.
    NoMetadata,
    /// The confirmed slot does not hold the confirmed image.
    NoValidImage,
}

/// The bootloader's decision.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Boot {
    /// Jump to this slot. `trial` is true if this is a trial boot.
    Slot {
        slot: Slot,
        version: u32,
        trial: bool,
    },
    Brick(Brick),
}

/// The bootloader: decides what to boot, spending a trial attempt or rolling
/// back first if it has to. Any metadata write it makes happens before it
/// returns, so a power cut during it leaves the previous record in force.
pub fn boot<F: Flash>(
    flash: &mut F,
    layout: &Layout,
    key: &VerifyingKey,
    cfg: &Config,
) -> Result<Boot, F::Error> {
    let Some(mut cur) = read_meta(flash, layout, cfg)? else {
        return Ok(Boot::Brick(Brick::NoMetadata));
    };
    if let Some(t) = cur.0.trial {
        let good = t.attempts_left > 0
            && verify_slot(flash, layout, t.slot, key)?.is_some_and(|i| i.version == t.version);
        if good {
            if !cfg.has(Mutant::DecrementAfterBoot) {
                let mut next = cur.0;
                next.trial = Some(Trial {
                    attempts_left: t.attempts_left - 1,
                    ..t
                });
                write_meta(flash, layout, cfg, cur, next)?;
            }
            return Ok(Boot::Slot {
                slot: t.slot,
                version: t.version,
                trial: true,
            });
        }
        // Out of attempts, or the trial slot does not hold the trial image:
        // roll back, and refuse this version from now on.
        let mut next = cur.0;
        next.trial = None;
        if !cfg.has(Mutant::RollbackWithoutReject) {
            next.rejected_version = t.version;
        }
        cur = write_meta(flash, layout, cfg, cur, next)?;
    }
    let m = cur.0;
    match verify_slot(flash, layout, m.confirmed, key)? {
        Some(i) if i.version == m.confirmed_version => Ok(Boot::Slot {
            slot: m.confirmed,
            version: m.confirmed_version,
            trial: false,
        }),
        _ => Ok(Boot::Brick(Brick::NoValidImage)),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpdateError<E> {
    Flash(E),
    NoMetadata,
    /// A trial is waiting to be confirmed or rolled back.
    TrialPending,
    /// The image is not well formed, not signed by the key, or does not fit.
    BadImage,
    /// The version is not newer than the confirmed one.
    NotNewer,
    /// This version was rolled back before.
    Rejected,
    /// The slot did not read back as the image that was written.
    WriteFailed,
}

impl<E> From<E> for UpdateError<E> {
    fn from(e: E) -> Self {
        UpdateError::Flash(e)
    }
}

/// Writes a new image into the slot that is not confirmed, verifies it, and
/// marks it as the trial. The confirmed slot is never touched.
///
/// The body is written before the header, so the slot holds no valid image
/// until the last page lands; the metadata write that starts the trial comes
/// after the read-back check.
pub fn update<F: Flash>(
    flash: &mut F,
    layout: &Layout,
    key: &VerifyingKey,
    cfg: &Config,
    image: &[u8],
) -> Result<Slot, UpdateError<F::Error>> {
    let cur = read_meta(flash, layout, cfg)?.ok_or(UpdateError::NoMetadata)?;
    if cur.0.trial.is_some() {
        return Err(UpdateError::TrialPending);
    }
    let info = verify_image(image, layout, key).ok_or(UpdateError::BadImage)?;
    if info.version <= cur.0.confirmed_version {
        return Err(UpdateError::NotNewer);
    }
    if info.version == cur.0.rejected_version {
        return Err(UpdateError::Rejected);
    }
    let target = cur.0.confirmed.other();
    let mut cur = cur;
    let trial = Trial {
        slot: target,
        version: info.version,
        attempts_left: cfg.max_attempts,
    };
    if cfg.has(Mutant::TrialBeforeImage) {
        let mut next = cur.0;
        next.trial = Some(trial);
        cur = write_meta(flash, layout, cfg, cur, next)?;
    }
    write_image(flash, layout, target, image)?;
    if verify_slot(flash, layout, target, key)? != Some(info) {
        return Err(UpdateError::WriteFailed);
    }
    if !cfg.has(Mutant::TrialBeforeImage) {
        let mut next = cur.0;
        next.trial = Some(trial);
        write_meta(flash, layout, cfg, cur, next)?;
    }
    Ok(target)
}

/// Erases the sectors an image needs in `slot`, then programs the body pages
/// and finally the header page.
pub fn write_image<F: Flash>(
    flash: &mut F,
    layout: &Layout,
    slot: Slot,
    image: &[u8],
) -> Result<(), F::Error> {
    let base = layout.slots[slot.index()];
    let len = image.len() as u32;
    let mut a = 0;
    while a < len {
        flash.erase_sector(base + a)?;
        a += layout.sector;
    }
    let mut off = HEADER_LEN;
    while off < len {
        let n = (len - off).min(layout.page);
        flash.program(base + off, &image[off as usize..(off + n) as usize])?;
        off += n;
    }
    flash.program(base, &image[..HEADER_LEN as usize])?;
    Ok(())
}

/// Called by firmware that has started and passed its own health checks.
/// If it is running as the trial, it becomes the confirmed image. Returns
/// whether anything changed.
pub fn confirm<F: Flash>(
    flash: &mut F,
    layout: &Layout,
    cfg: &Config,
    running: Slot,
) -> Result<bool, UpdateError<F::Error>> {
    let cur = read_meta(flash, layout, cfg)?.ok_or(UpdateError::NoMetadata)?;
    match cur.0.trial {
        Some(t) if t.slot == running => {
            let mut next = cur.0;
            next.confirmed = t.slot;
            next.confirmed_version = t.version;
            next.trial = None;
            write_meta(flash, layout, cfg, cur, next)?;
            Ok(true)
        }
        _ => Ok(false),
    }
}

/// Factory provisioning: erases everything, writes `image` to slot A as the
/// confirmed image, and writes the first metadata record.
pub fn provision<F: Flash>(
    flash: &mut F,
    layout: &Layout,
    key: &VerifyingKey,
    image: &[u8],
) -> Result<(), UpdateError<F::Error>> {
    let info = verify_image(image, layout, key).ok_or(UpdateError::BadImage)?;
    for addr in layout.meta {
        flash.erase_sector(addr)?;
    }
    for base in layout.slots {
        let mut a = 0;
        while a < layout.slot_size {
            flash.erase_sector(base + a)?;
            a += layout.sector;
        }
    }
    write_image(flash, layout, Slot::A, image)?;
    let m = Meta {
        seq: 1,
        confirmed: Slot::A,
        confirmed_version: info.version,
        trial: None,
        rejected_version: 0,
    };
    flash.program(layout.meta[0], &m.encode())?;
    Ok(())
}

/// Builds the header page for an image body: magic, version, length, body
/// hash, and the slot for the signature, which the caller fills in by
/// signing `header[..SIGNED_LEN]`.
pub fn unsigned_header(version: u32, body: &[u8]) -> [u8; HEADER_LEN as usize] {
    let mut h = [0xFFu8; HEADER_LEN as usize];
    h[0..4].copy_from_slice(&IMAGE_MAGIC);
    h[4..8].copy_from_slice(&version.to_le_bytes());
    h[8..12].copy_from_slice(&(body.len() as u32).to_le_bytes());
    let digest: [u8; 32] = Sha256::digest(body).into();
    h[12..44].copy_from_slice(&digest);
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_matches_the_standard_check_value() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn meta_round_trips_and_rejects_any_single_bit_flip() {
        let m = Meta {
            seq: 7,
            confirmed: Slot::B,
            confirmed_version: 2,
            trial: Some(Trial {
                slot: Slot::A,
                version: 3,
                attempts_left: 2,
            }),
            rejected_version: 0,
        };
        let b = m.encode();
        assert_eq!(Meta::decode(&b), Some(m));
        for bit in 0..META_LEN * 8 {
            let mut c = b;
            c[bit / 8] ^= 1 << (bit % 8);
            assert_eq!(Meta::decode(&c), None, "bit {bit}");
        }
    }

    #[test]
    fn erased_flash_is_not_a_record() {
        assert_eq!(Meta::decode(&[0xFF; META_LEN]), None);
        assert_eq!(Meta::decode(&[0; META_LEN]), None);
    }

    #[test]
    fn standard_layout_does_not_overlap() {
        let l = Layout::standard();
        assert_eq!(l.size(), 40960);
        assert!(l.meta[1] + l.sector <= l.slots[0]);
        assert!(l.slots[0] + l.slot_size <= l.slots[1]);
    }
}
