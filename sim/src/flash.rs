//! A NOR flash model that can lose power in the middle of any operation.
//!
//! Erasing sets a whole sector to 0xFF. Programming can only clear bits, so
//! writing over data that is not erased ANDs the two. A power cut is armed at
//! the n-th erase or program since power-on; when it fires, the operation
//! leaves whatever a real part might leave, and every later operation fails
//! until [`SimFlash::power_on`].

use ota_safe::{Flash, Layout};
use std::fmt;

/// The error every flash operation returns once power is gone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PowerLoss;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum OpKind {
    Erase,
    Program,
}

/// One erase or program, as it was asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Op {
    pub kind: OpKind,
    pub addr: u32,
    pub len: u32,
}

impl fmt::Display for Op {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self.kind {
            OpKind::Erase => write!(f, "erase sector 0x{:05x}", self.addr),
            OpKind::Program => write!(f, "program {} B at 0x{:05x}", self.len, self.addr),
        }
    }
}

/// What the interrupted operation leaves behind.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Tear {
    /// Power went before the operation started (or just after the one
    /// before it finished).
    Before,
    /// A program that got through the first `n` quarters of its bytes; the
    /// next byte has only some of its bits programmed.
    Prefix(u8),
    /// A program where each bit that should have cleared did or did not.
    Scatter,
    /// An erase that stopped part-way: each byte is either erased or has
    /// only some of its bits set back to 1. Two different seeds.
    Erase(u8),
}

impl Tear {
    /// The tears a power cut at this kind of operation can leave.
    pub fn for_kind(kind: OpKind) -> &'static [Tear] {
        match kind {
            OpKind::Program => &[
                Tear::Before,
                Tear::Prefix(1),
                Tear::Prefix(2),
                Tear::Prefix(3),
                Tear::Scatter,
            ],
            OpKind::Erase => &[Tear::Before, Tear::Erase(0), Tear::Erase(1)],
        }
    }
}

impl fmt::Display for Tear {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Tear::Before => write!(f, "before"),
            Tear::Prefix(q) => write!(f, "prefix-{q}"),
            Tear::Scatter => write!(f, "scatter"),
            Tear::Erase(s) => write!(f, "erase-{s}"),
        }
    }
}

impl std::str::FromStr for Tear {
    type Err = String;
    fn from_str(s: &str) -> Result<Tear, String> {
        Ok(match s {
            "before" => Tear::Before,
            "prefix-1" => Tear::Prefix(1),
            "prefix-2" => Tear::Prefix(2),
            "prefix-3" => Tear::Prefix(3),
            "scatter" => Tear::Scatter,
            "erase-0" => Tear::Erase(0),
            "erase-1" => Tear::Erase(1),
            _ => {
                return Err(format!(
                    "unknown tear {s:?}: one of before, prefix-1, prefix-2, prefix-3, scatter, erase-0, erase-1"
                ))
            }
        })
    }
}

/// A power cut at the `at`-th erase or program after power-on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Cut {
    pub at: usize,
    pub tear: Tear,
}

impl fmt::Display for Cut {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}:{}", self.at, self.tear)
    }
}

impl std::str::FromStr for Cut {
    type Err = String;
    fn from_str(s: &str) -> Result<Cut, String> {
        let (at, tear) = s.split_once(':').unwrap_or((s, "before"));
        Ok(Cut {
            at: at
                .parse()
                .map_err(|_| format!("bad op index in cut {s:?}"))?,
            tear: tear.parse()?,
        })
    }
}

#[derive(Clone)]
pub struct SimFlash {
    pub mem: Vec<u8>,
    layout: Layout,
    count: usize,
    cut: Option<Cut>,
    dead: bool,
    /// Every operation since power-on, including the one that was cut.
    pub ops: Vec<Op>,
}

impl SimFlash {
    /// Factory-fresh flash: everything erased.
    pub fn new(layout: Layout) -> SimFlash {
        SimFlash {
            mem: vec![0xFF; layout.size() as usize],
            layout,
            count: 0,
            cut: None,
            dead: false,
            ops: Vec::new(),
        }
    }

    /// Restores power. The next operation is number 0 again.
    pub fn power_on(&mut self) {
        self.dead = false;
        self.cut = None;
        self.count = 0;
        self.ops.clear();
    }

    pub fn arm(&mut self, cut: Cut) {
        self.cut = Some(cut);
    }

    /// Operations since power-on.
    pub fn count(&self) -> usize {
        self.count
    }

    fn step(&mut self, op: Op) -> Result<Option<Tear>, PowerLoss> {
        if self.dead {
            return Err(PowerLoss);
        }
        self.ops.push(op);
        let n = self.count;
        self.count += 1;
        match self.cut {
            Some(c) if c.at == n => {
                self.dead = true;
                Ok(Some(c.tear))
            }
            _ => Ok(None),
        }
    }

    fn rng(&self, salt: u64) -> Rng {
        let c = self.cut.map_or(0, |c| c.at as u64);
        Rng(0x9E37_79B9_7F4A_7C15 ^ (c << 20) ^ salt)
    }
}

impl Flash for SimFlash {
    type Error = PowerLoss;

    fn read(&mut self, addr: u32, buf: &mut [u8]) -> Result<(), PowerLoss> {
        if self.dead {
            return Err(PowerLoss);
        }
        let a = addr as usize;
        buf.copy_from_slice(&self.mem[a..a + buf.len()]);
        Ok(())
    }

    fn erase_sector(&mut self, addr: u32) -> Result<(), PowerLoss> {
        assert_eq!(
            addr % self.layout.sector,
            0,
            "erase of unaligned 0x{addr:x}"
        );
        assert!(addr < self.layout.size(), "erase past the end: 0x{addr:x}");
        let op = Op {
            kind: OpKind::Erase,
            addr,
            len: self.layout.sector,
        };
        let range = addr as usize..(addr + self.layout.sector) as usize;
        match self.step(op)? {
            None => {
                self.mem[range].fill(0xFF);
                Ok(())
            }
            Some(Tear::Erase(seed)) => {
                let mut r = self.rng(0xE5 + seed as u64);
                for b in &mut self.mem[range] {
                    let x = r.next_u64();
                    *b = if x & 1 == 0 {
                        0xFF
                    } else {
                        *b | (x >> 8) as u8
                    };
                }
                Err(PowerLoss)
            }
            Some(_) => Err(PowerLoss),
        }
    }

    fn program(&mut self, addr: u32, data: &[u8]) -> Result<(), PowerLoss> {
        let page = self.layout.page;
        assert!(!data.is_empty());
        assert_eq!(
            addr / page,
            (addr + data.len() as u32 - 1) / page,
            "program crosses a page: 0x{addr:x} + {}",
            data.len()
        );
        let op = Op {
            kind: OpKind::Program,
            addr,
            len: data.len() as u32,
        };
        let a = addr as usize;
        match self.step(op)? {
            None => {
                for (m, d) in self.mem[a..a + data.len()].iter_mut().zip(data) {
                    *m &= d;
                }
                Ok(())
            }
            Some(Tear::Prefix(q)) => {
                let k = data.len() * q as usize / 4;
                for (m, d) in self.mem[a..a + k].iter_mut().zip(data) {
                    *m &= d;
                }
                if k < data.len() {
                    let x = self.rng(0x9F).next_u64() as u8;
                    self.mem[a + k] &= data[k] | x;
                }
                Err(PowerLoss)
            }
            Some(Tear::Scatter) => {
                let mut r = self.rng(0x5C);
                for (i, d) in data.iter().enumerate() {
                    self.mem[a + i] &= d | r.next_u64() as u8;
                }
                Err(PowerLoss)
            }
            Some(_) => Err(PowerLoss),
        }
    }
}

/// splitmix64: deterministic, so every cut replays exactly.
pub struct Rng(pub u64);

impl Rng {
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flash() -> SimFlash {
        SimFlash::new(Layout::standard())
    }

    #[test]
    fn program_only_clears_bits() {
        let mut f = flash();
        f.program(0, &[0b1010_1010]).unwrap();
        f.program(0, &[0b1100_1100]).unwrap();
        assert_eq!(f.mem[0], 0b1000_1000);
        f.erase_sector(0).unwrap();
        assert_eq!(f.mem[0], 0xFF);
    }

    #[test]
    #[should_panic(expected = "crosses a page")]
    fn program_may_not_cross_a_page() {
        flash().program(250, &[0; 10]).unwrap();
    }

    #[test]
    fn a_cut_fires_at_its_operation_and_power_stays_off() {
        let mut f = flash();
        f.arm(Cut {
            at: 1,
            tear: Tear::Before,
        });
        f.program(0, &[0]).unwrap();
        assert_eq!(f.program(1, &[0]), Err(PowerLoss));
        assert_eq!(f.mem[1], 0xFF);
        assert_eq!(f.program(2, &[0]), Err(PowerLoss));
        let mut b = [0];
        assert_eq!(f.read(0, &mut b), Err(PowerLoss));
        f.power_on();
        f.program(2, &[0]).unwrap();
        assert_eq!(f.count(), 1);
    }

    #[test]
    fn torn_program_never_sets_a_bit_and_never_finishes() {
        for tear in Tear::for_kind(OpKind::Program) {
            let mut f = flash();
            let data: Vec<u8> = (0..256).map(|i| (i * 37) as u8).collect();
            f.arm(Cut { at: 0, tear: *tear });
            assert_eq!(f.program(0, &data), Err(PowerLoss));
            let got = &f.mem[..256];
            assert!(
                got.iter().zip(&data).all(|(g, d)| g & d == *d),
                "{tear}: a bit went the wrong way"
            );
            if *tear == Tear::Before {
                assert!(got.iter().all(|&b| b == 0xFF));
            } else {
                assert_ne!(got, &data[..], "{tear}");
            }
        }
    }

    #[test]
    fn torn_erase_leaves_a_mix() {
        let mut f = flash();
        f.program(0, &[0; 256]).unwrap();
        f.power_on();
        f.arm(Cut {
            at: 0,
            tear: Tear::Erase(0),
        });
        assert_eq!(f.erase_sector(0), Err(PowerLoss));
        let s = &f.mem[..4096];
        assert!(s.contains(&0xFF));
        assert!(s.iter().any(|&b| b != 0xFF));
    }

    #[test]
    fn cuts_parse_and_print() {
        let c: Cut = "57:prefix-3".parse().unwrap();
        assert_eq!(
            c,
            Cut {
                at: 57,
                tear: Tear::Prefix(3)
            }
        );
        assert_eq!(c.to_string(), "57:prefix-3");
        assert_eq!("9".parse::<Cut>().unwrap().tear, Tear::Before);
        assert!("x:before".parse::<Cut>().is_err());
    }
}
