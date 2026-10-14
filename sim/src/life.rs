//! A device's life: boot, run the firmware, which updates or confirms, and
//! reboot, until it settles. Power can be cut anywhere along the way; after
//! each cut the device powers on again and carries on.
//!
//! The firmware behaves the same in every scheme:
//!
//! - v1 installs v2 and reboots;
//! - v2 confirms itself, installs v3 and reboots;
//! - v3 confirms itself and settles, or, in the bad-update workload, crashes
//!   before confirming, every time it starts.
//!
//! After every boot the checker asks three questions, using its own reading
//! of the flash rather than what the bootloader claims:
//!
//! - the booted slot holds a valid, signed image of the version booted;
//! - that version is not older than one the firmware already confirmed (or,
//!   for schemes with no trial, already committed);
//! - an unconfirmed version has not booted more often than the scheme's trial
//!   limit allows.
//!
//! At the end, the device must have settled on the expected version: v3, or
//! v2 with v3 rolled back in the bad-update workload.

use crate::flash::{Cut, Op, PowerLoss, SimFlash};
use crate::scheme::{verifying_key, Booted, Images, Scheme, Updated};
use ota_safe as ota;
use std::collections::BTreeMap;
use std::fmt;

/// Boots allowed in one life before it counts as stuck.
pub const MAX_BOOTS: u32 = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Workload {
    /// Every version works.
    Good,
    /// v3 crashes before confirming itself, every time.
    BadUpdate,
}

impl Workload {
    pub fn expected_end(self) -> u32 {
        match self {
            Workload::Good => 3,
            Workload::BadUpdate => 2,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Workload::Good => "good",
            Workload::BadUpdate => "bad-update",
        }
    }
}

impl std::str::FromStr for Workload {
    type Err = String;
    fn from_str(s: &str) -> Result<Workload, String> {
        match s {
            "good" => Ok(Workload::Good),
            "bad-update" | "bad" => Ok(Workload::BadUpdate),
            _ => Err(format!("unknown workload {s:?}: good or bad-update")),
        }
    }
}

/// How a life ended, worst first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Booted something it must not have.
    Unsafe(String),
    /// The bootloader found nothing it could boot.
    Bricked(String),
    /// Never settled on the expected version.
    Stuck(String),
    Ok,
}

impl Outcome {
    pub fn rank(&self) -> u8 {
        match self {
            Outcome::Unsafe(_) => 3,
            Outcome::Bricked(_) => 2,
            Outcome::Stuck(_) => 1,
            Outcome::Ok => 0,
        }
    }
    pub fn label(&self) -> &'static str {
        match self {
            Outcome::Unsafe(_) => "UNSAFE",
            Outcome::Bricked(_) => "BRICKED",
            Outcome::Stuck(_) => "STUCK",
            Outcome::Ok => "ok",
        }
    }
    pub fn detail(&self) -> &str {
        match self {
            Outcome::Unsafe(s) | Outcome::Bricked(s) | Outcome::Stuck(s) => s,
            Outcome::Ok => "",
        }
    }
}

impl fmt::Display for Outcome {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Outcome::Ok => write!(f, "ok"),
            o => write!(f, "{}: {}", o.label(), o.detail()),
        }
    }
}

/// Something that happened, and how many flash operations into its power-on
/// period it happened.
#[derive(Clone, Debug)]
pub struct Event {
    pub period: usize,
    pub op: usize,
    pub what: String,
}

pub struct Life {
    pub outcome: Outcome,
    /// The flash as the first power cut left it, if asked for.
    pub at_first_cut: Option<SimFlash>,
    pub events: Vec<Event>,
    /// The operations of each power-on period, the cut one included.
    pub periods: Vec<Vec<Op>>,
}

/// Provisions a fresh device for `scheme` with v1.
pub fn provisioned(scheme: &dyn Scheme, images: &Images) -> SimFlash {
    let mut f = SimFlash::new(scheme.layout());
    scheme.provision(&mut f, images.get(1));
    f.power_on();
    f
}

enum Period {
    /// The firmware settled on this version.
    Settled(u32),
    Reboot,
    Done(Outcome),
}

struct Run<'a> {
    scheme: &'a dyn Scheme,
    images: &'a Images,
    workload: Workload,
    f: SimFlash,
    /// Highest version confirmed (or committed) with the call returning.
    acked: u32,
    unconfirmed_boots: BTreeMap<u32, u32>,
    boots: u32,
    period: usize,
    events: Vec<Event>,
}

impl Run<'_> {
    fn note(&mut self, what: String) {
        self.events.push(Event {
            period: self.period,
            op: self.f.count(),
            what,
        });
    }

    fn period(&mut self) -> Result<Period, PowerLoss> {
        self.boots += 1;
        if self.boots > MAX_BOOTS {
            return Ok(Period::Done(Outcome::Stuck(format!(
                "still rebooting after {MAX_BOOTS} boots"
            ))));
        }
        let (slot, version) = match self.scheme.boot(&mut self.f)? {
            Booted::Brick(why) => {
                self.note(format!("boot: BRICK ({why})"));
                return Ok(Period::Done(Outcome::Bricked(why.into())));
            }
            Booted::Slot { slot, version } => (slot, version),
        };
        self.note(format!("boot v{version} from slot {slot:?}"));
        if let Some(o) = self.check_boot(slot, version)? {
            return Ok(Period::Done(o));
        }
        let next = version + 1;
        if version == 2 && self.scheme.confirm(&mut self.f, slot)? {
            self.acked = self.acked.max(2);
            self.note("v2 confirmed".into());
        }
        if version == 3 {
            return Ok(match self.workload {
                Workload::BadUpdate => {
                    self.note("v3 crashes before confirming".into());
                    Period::Reboot
                }
                Workload::Good => {
                    if self.scheme.confirm(&mut self.f, slot)? {
                        self.acked = self.acked.max(3);
                        self.note("v3 confirmed".into());
                    }
                    Period::Settled(3)
                }
            });
        }
        self.note(format!("v{version} installs v{next}"));
        match self.scheme.update(&mut self.f, self.images.get(next))? {
            Updated::Staged => {
                self.note(format!("v{next} written; reboot into trial"));
                Ok(Period::Reboot)
            }
            Updated::Committed => {
                self.acked = self.acked.max(next);
                self.note(format!("v{next} written and active; reboot"));
                Ok(Period::Reboot)
            }
            Updated::Refused(why) => {
                self.note(format!("v{next} refused ({why}); settle on v{version}"));
                Ok(Period::Settled(version))
            }
        }
    }

    fn check_boot(&mut self, slot: ota::Slot, version: u32) -> Result<Option<Outcome>, PowerLoss> {
        let layout = self.scheme.layout();
        let held = ota::verify_slot(&mut self.f, &layout, slot, &verifying_key())?;
        if held.map(|i| i.version) != Some(version) {
            return Ok(Some(Outcome::Unsafe(format!(
                "booted v{version} from slot {slot:?}, which does not hold a valid signed v{version}"
            ))));
        }
        if version < self.acked {
            return Ok(Some(Outcome::Unsafe(format!(
                "booted v{version} after v{} was confirmed",
                self.acked
            ))));
        }
        if version > self.acked {
            let n = self.unconfirmed_boots.entry(version).or_insert(0);
            *n += 1;
            if let Some(limit) = self.scheme.max_attempts() {
                if *n > limit as u32 {
                    return Ok(Some(Outcome::Unsafe(format!(
                        "unconfirmed v{version} booted {n} times (limit {limit})"
                    ))));
                }
            }
        }
        Ok(None)
    }
}

/// Lives one life from a freshly provisioned device. `cuts[0]` is armed at
/// first power-on, `cuts[1]` after the device powers on again, and so on.
pub fn live(
    scheme: &dyn Scheme,
    images: &Images,
    fresh: &SimFlash,
    workload: Workload,
    cuts: &[Cut],
) -> Life {
    live_inner(scheme, images, fresh, workload, cuts, false)
}

/// [`live`], also keeping the flash exactly as the first cut left it.
pub fn live_keeping_cut(
    scheme: &dyn Scheme,
    images: &Images,
    fresh: &SimFlash,
    workload: Workload,
    cuts: &[Cut],
) -> Life {
    live_inner(scheme, images, fresh, workload, cuts, true)
}

fn live_inner(
    scheme: &dyn Scheme,
    images: &Images,
    fresh: &SimFlash,
    workload: Workload,
    cuts: &[Cut],
    keep: bool,
) -> Life {
    let mut at_first_cut = None;
    let mut run = Run {
        scheme,
        images,
        workload,
        f: fresh.clone(),
        acked: 1,
        unconfirmed_boots: BTreeMap::new(),
        boots: 0,
        period: 0,
        events: Vec::new(),
    };
    let mut periods = Vec::new();
    let mut cuts = cuts.iter();
    if let Some(c) = cuts.next() {
        run.f.arm(*c);
    }
    let outcome = loop {
        match run.period() {
            Ok(Period::Reboot) => {}
            Ok(Period::Settled(v)) => {
                let want = workload.expected_end();
                break if v == want {
                    Outcome::Ok
                } else {
                    Outcome::Stuck(format!("settled on v{v}, expected v{want}"))
                };
            }
            Ok(Period::Done(o)) => break o,
            Err(PowerLoss) => {
                let op = run.f.ops.last().copied();
                let what = match op {
                    Some(op) => format!("POWER CUT at op {} ({op})", run.f.count() - 1),
                    None => "POWER CUT".into(),
                };
                run.events.push(Event {
                    period: run.period,
                    op: run.f.count().saturating_sub(1),
                    what,
                });
                if keep && at_first_cut.is_none() {
                    at_first_cut = Some(run.f.clone());
                }
                periods.push(std::mem::take(&mut run.f.ops));
                run.f.power_on();
                run.period += 1;
                if let Some(c) = cuts.next() {
                    run.f.arm(*c);
                }
            }
        }
    };
    periods.push(std::mem::take(&mut run.f.ops));
    Life {
        outcome,
        at_first_cut,
        events: run.events,
        periods,
    }
}
