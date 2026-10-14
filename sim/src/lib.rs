//! The power-cut simulator behind `ota-sim`: a NOR flash model that can lose
//! power mid-operation, the update schemes under test, a device life, and the
//! search over every cut.

pub mod flash;
pub mod life;
pub mod scheme;
pub mod search;
pub mod svg;
