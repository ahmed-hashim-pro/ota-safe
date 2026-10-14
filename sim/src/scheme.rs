//! The update schemes under test: ota-safe itself (optionally with a planted
//! bug), and two naive designs to compare it with.

use crate::flash::{PowerLoss, SimFlash};
use ed25519_dalek::{Signer, SigningKey, VerifyingKey};
use ota_safe::{self as ota, Config, Flash, Layout, Mutant, Slot, UpdateError};

/// The signing key every simulated image is signed with. A fixed seed, so
/// images and results are the same on every run.
pub fn signing_key() -> SigningKey {
    SigningKey::from_bytes(&[7u8; 32])
}

pub fn verifying_key() -> VerifyingKey {
    signing_key().verifying_key()
}

/// A signed image of `version` with a `body_len`-byte body whose content is
/// derived from the version.
pub fn make_image(version: u32, body_len: usize) -> Vec<u8> {
    let mut rng = crate::flash::Rng(0x1234_5678 ^ version as u64);
    let body: Vec<u8> = (0..body_len).map(|_| rng.next_u64() as u8).collect();
    let mut header = ota::unsigned_header(version, &body);
    let sig = signing_key().sign(&header[..ota::SIGNED_LEN]);
    header[ota::SIGNED_LEN..ota::SIGNED_LEN + 64].copy_from_slice(&sig.to_bytes());
    let mut img = header.to_vec();
    img.extend_from_slice(&body);
    img
}

/// The three firmware versions every simulated device goes through.
pub struct Images {
    pub v: [Vec<u8>; 3],
}

impl Images {
    pub fn standard() -> Images {
        Images {
            v: [
                make_image(1, 9000),
                make_image(2, 10000),
                make_image(3, 11000),
            ],
        }
    }
    pub fn get(&self, version: u32) -> &[u8] {
        &self.v[version as usize - 1]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Booted {
    Slot { slot: Slot, version: u32 },
    Brick(&'static str),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Updated {
    /// Written; it will boot as a trial and must be confirmed.
    Staged,
    /// Written, and it is now what the device boots. No way back.
    Committed,
    /// The scheme declined (not newer, rejected before, trial pending...).
    Refused(&'static str),
}

pub trait Scheme: Sync {
    fn name(&self) -> String;
    fn layout(&self) -> Layout {
        Layout::standard()
    }
    fn provision(&self, f: &mut SimFlash, image: &[u8]);
    fn boot(&self, f: &mut SimFlash) -> Result<Booted, PowerLoss>;
    fn update(&self, f: &mut SimFlash, image: &[u8]) -> Result<Updated, PowerLoss>;
    /// Returns whether the running image became the confirmed one.
    fn confirm(&self, f: &mut SimFlash, running: Slot) -> Result<bool, PowerLoss>;
    /// Trial boots allowed before a rollback, if the scheme has trials.
    fn max_attempts(&self) -> Option<u8> {
        None
    }
}

/// ota-safe, through the same functions firmware would call.
pub struct OtaSafe {
    pub cfg: Config,
}

impl OtaSafe {
    pub fn new() -> OtaSafe {
        OtaSafe {
            cfg: Config::default(),
        }
    }
    pub fn with_mutant(mutant: Mutant) -> OtaSafe {
        OtaSafe {
            cfg: Config {
                mutant,
                ..Config::default()
            },
        }
    }
}

impl Default for OtaSafe {
    fn default() -> Self {
        Self::new()
    }
}

/// Why ota-safe refused, or the power loss that stopped it.
fn refusal(e: UpdateError<PowerLoss>) -> Result<&'static str, PowerLoss> {
    match e {
        UpdateError::Flash(p) => Err(p),
        UpdateError::NoMetadata => Ok("no metadata"),
        UpdateError::TrialPending => Ok("a trial is pending"),
        UpdateError::BadImage => Ok("bad image"),
        UpdateError::NotNewer => Ok("not newer than the confirmed version"),
        UpdateError::Rejected => Ok("rolled back before"),
        UpdateError::WriteFailed => Ok("slot did not read back"),
    }
}

impl Scheme for OtaSafe {
    fn name(&self) -> String {
        match self.cfg.mutant {
            Mutant::None => "ota-safe".into(),
            m => mutant_name(m).into(),
        }
    }

    fn provision(&self, f: &mut SimFlash, image: &[u8]) {
        ota::provision(f, &self.layout(), &verifying_key(), image).expect("provisioning");
    }

    fn boot(&self, f: &mut SimFlash) -> Result<Booted, PowerLoss> {
        Ok(
            match ota::boot(f, &self.layout(), &verifying_key(), &self.cfg)? {
                ota::Boot::Slot { slot, version, .. } => Booted::Slot { slot, version },
                ota::Boot::Brick(ota::Brick::NoMetadata) => {
                    Booted::Brick("no valid metadata record")
                }
                ota::Boot::Brick(ota::Brick::NoValidImage) => {
                    Booted::Brick("confirmed slot does not hold the confirmed image")
                }
            },
        )
    }

    fn update(&self, f: &mut SimFlash, image: &[u8]) -> Result<Updated, PowerLoss> {
        match ota::update(f, &self.layout(), &verifying_key(), &self.cfg, image) {
            Ok(_) => Ok(Updated::Staged),
            Err(e) => refusal(e).map(Updated::Refused),
        }
    }

    fn confirm(&self, f: &mut SimFlash, running: Slot) -> Result<bool, PowerLoss> {
        match ota::confirm(f, &self.layout(), &self.cfg, running) {
            Ok(changed) => Ok(changed),
            Err(e) => refusal(e).map(|_| false),
        }
    }

    fn max_attempts(&self) -> Option<u8> {
        Some(self.cfg.max_attempts)
    }
}

pub const MUTANTS: [Mutant; 6] = [
    Mutant::SingleMetaCopy,
    Mutant::NoCrc,
    Mutant::IgnoreSeq,
    Mutant::DecrementAfterBoot,
    Mutant::RollbackWithoutReject,
    Mutant::TrialBeforeImage,
];

pub fn mutant_name(m: Mutant) -> &'static str {
    match m {
        Mutant::None => "none",
        Mutant::SingleMetaCopy => "single-meta-copy",
        Mutant::NoCrc => "no-crc",
        Mutant::IgnoreSeq => "ignore-seq",
        Mutant::DecrementAfterBoot => "decrement-after-boot",
        Mutant::RollbackWithoutReject => "rollback-without-reject",
        Mutant::TrialBeforeImage => "trial-before-image",
    }
}

pub fn mutant_doc(m: Mutant) -> &'static str {
    match m {
        Mutant::None => "",
        Mutant::SingleMetaCopy => "metadata always rewritten in place in one sector",
        Mutant::NoCrc => "metadata CRC not checked",
        Mutant::IgnoreSeq => "first valid metadata record wins, not the newest",
        Mutant::DecrementAfterBoot => "trial attempt spent by the app after it starts",
        Mutant::RollbackWithoutReject => "rolled-back version not recorded as rejected",
        Mutant::TrialBeforeImage => "trial marked before the image is written",
    }
}

/// One slot, rewritten in place. The bootloader verifies the signature and
/// refuses to boot a bad image, which is all it can do.
pub struct NaiveInPlace;

impl Scheme for NaiveInPlace {
    fn name(&self) -> String {
        "naive in-place".into()
    }
    fn provision(&self, f: &mut SimFlash, image: &[u8]) {
        let l = self.layout();
        ota::write_image(f, &l, Slot::A, image).expect("provisioning");
    }
    fn boot(&self, f: &mut SimFlash) -> Result<Booted, PowerLoss> {
        Ok(
            match ota::verify_slot(f, &self.layout(), Slot::A, &verifying_key())? {
                Some(i) => Booted::Slot {
                    slot: Slot::A,
                    version: i.version,
                },
                None => Booted::Brick("the only slot does not hold a valid image"),
            },
        )
    }
    fn update(&self, f: &mut SimFlash, image: &[u8]) -> Result<Updated, PowerLoss> {
        ota::write_image(f, &self.layout(), Slot::A, image)?;
        Ok(Updated::Committed)
    }
    fn confirm(&self, _: &mut SimFlash, _: Slot) -> Result<bool, PowerLoss> {
        Ok(false)
    }
}

/// Two slots and a one-byte "active slot" flag at the start of the first
/// metadata sector, erased and rewritten to switch. Images are verified at
/// boot; there is no trial, no rollback and no second copy of the flag.
pub struct NaiveAB;

impl NaiveAB {
    fn flag(&self, f: &mut SimFlash) -> Result<Option<Slot>, PowerLoss> {
        let mut b = [0u8];
        f.read(self.layout().meta[0], &mut b)?;
        Ok(match b[0] {
            0 => Some(Slot::A),
            1 => Some(Slot::B),
            _ => None,
        })
    }
}

impl Scheme for NaiveAB {
    fn name(&self) -> String {
        "naive A/B".into()
    }
    fn provision(&self, f: &mut SimFlash, image: &[u8]) {
        let l = self.layout();
        ota::write_image(f, &l, Slot::A, image).expect("provisioning");
        f.program(l.meta[0], &[0]).expect("provisioning");
    }
    fn boot(&self, f: &mut SimFlash) -> Result<Booted, PowerLoss> {
        let Some(slot) = self.flag(f)? else {
            return Ok(Booted::Brick("active-slot flag is neither A nor B"));
        };
        Ok(
            match ota::verify_slot(f, &self.layout(), slot, &verifying_key())? {
                Some(i) => Booted::Slot {
                    slot,
                    version: i.version,
                },
                None => Booted::Brick("the active slot does not hold a valid image"),
            },
        )
    }
    fn update(&self, f: &mut SimFlash, image: &[u8]) -> Result<Updated, PowerLoss> {
        let l = self.layout();
        let Some(active) = self.flag(f)? else {
            return Ok(Updated::Refused("active-slot flag unreadable"));
        };
        let target = active.other();
        ota::write_image(f, &l, target, image)?;
        f.erase_sector(l.meta[0])?;
        f.program(l.meta[0], &[target.index() as u8])?;
        Ok(Updated::Committed)
    }
    fn confirm(&self, _: &mut SimFlash, _: Slot) -> Result<bool, PowerLoss> {
        Ok(false)
    }
}
