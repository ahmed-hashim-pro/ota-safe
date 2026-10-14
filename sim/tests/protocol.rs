//! ota-safe's behaviour with the power on: what the bootloader, updater and
//! confirm do, and what they refuse.

use ota_safe::{self as ota, Boot, Brick, Config, Flash, Layout, Meta, Slot, UpdateError};
use ota_sim::flash::SimFlash;
use ota_sim::scheme::{make_image, verifying_key};

fn device() -> (SimFlash, Layout, Config) {
    let l = Layout::standard();
    let mut f = SimFlash::new(l);
    ota::provision(&mut f, &l, &verifying_key(), &make_image(1, 9000)).unwrap();
    (f, l, Config::default())
}

fn boot(f: &mut SimFlash, l: &Layout, c: &Config) -> Boot {
    ota::boot(f, l, &verifying_key(), c).unwrap()
}

fn update(
    f: &mut SimFlash,
    l: &Layout,
    c: &Config,
    img: &[u8],
) -> Result<Slot, UpdateError<ota_sim::flash::PowerLoss>> {
    ota::update(f, l, &verifying_key(), c, img)
}

fn meta(f: &mut SimFlash, l: &Layout, c: &Config) -> Meta {
    ota::read_meta(f, l, c).unwrap().unwrap().0
}

fn slot(slot: Slot, version: u32, trial: bool) -> Boot {
    Boot::Slot {
        slot,
        version,
        trial,
    }
}

#[test]
fn a_provisioned_device_boots_v1_from_a() {
    let (mut f, l, c) = device();
    assert_eq!(boot(&mut f, &l, &c), slot(Slot::A, 1, false));
    assert_eq!(meta(&mut f, &l, &c).seq, 1);
}

#[test]
fn an_update_boots_as_a_trial_and_confirm_makes_it_stick() {
    let (mut f, l, c) = device();
    assert_eq!(update(&mut f, &l, &c, &make_image(2, 10000)), Ok(Slot::B));
    assert_eq!(boot(&mut f, &l, &c), slot(Slot::B, 2, true));
    let m = meta(&mut f, &l, &c);
    assert_eq!(
        m.trial.unwrap().attempts_left,
        2,
        "the attempt is spent before the boot"
    );
    assert_eq!(ota::confirm(&mut f, &l, &c, Slot::B), Ok(true));
    assert_eq!(boot(&mut f, &l, &c), slot(Slot::B, 2, false));
    let m = meta(&mut f, &l, &c);
    assert_eq!(
        (m.confirmed, m.confirmed_version, m.trial),
        (Slot::B, 2, None)
    );
    // A second update goes back into A.
    assert_eq!(update(&mut f, &l, &c, &make_image(3, 11000)), Ok(Slot::A));
}

#[test]
fn an_unconfirmed_trial_gets_three_boots_then_rolls_back_and_is_refused() {
    let (mut f, l, c) = device();
    update(&mut f, &l, &c, &make_image(2, 10000)).unwrap();
    for _ in 0..3 {
        assert_eq!(boot(&mut f, &l, &c), slot(Slot::B, 2, true));
    }
    assert_eq!(boot(&mut f, &l, &c), slot(Slot::A, 1, false));
    let m = meta(&mut f, &l, &c);
    assert_eq!((m.trial, m.rejected_version), (None, 2));
    assert_eq!(
        update(&mut f, &l, &c, &make_image(2, 10000)),
        Err(UpdateError::Rejected)
    );
    // A newer version is still accepted.
    assert_eq!(update(&mut f, &l, &c, &make_image(3, 11000)), Ok(Slot::B));
}

#[test]
fn confirm_from_the_wrong_slot_changes_nothing() {
    let (mut f, l, c) = device();
    update(&mut f, &l, &c, &make_image(2, 10000)).unwrap();
    boot(&mut f, &l, &c);
    let before = meta(&mut f, &l, &c);
    assert_eq!(ota::confirm(&mut f, &l, &c, Slot::A), Ok(false));
    assert_eq!(meta(&mut f, &l, &c), before);
}

#[test]
fn a_trial_slot_damaged_after_staging_is_rejected_not_booted() {
    let (mut f, l, c) = device();
    update(&mut f, &l, &c, &make_image(2, 10000)).unwrap();
    let a = (l.slots[1] + ota::HEADER_LEN + 5000) as usize;
    f.mem[a] &= 0x7F;
    if f.mem[a] == 0x7F {
        f.mem[a] = 0;
    }
    assert_eq!(boot(&mut f, &l, &c), slot(Slot::A, 1, false));
    assert_eq!(meta(&mut f, &l, &c).rejected_version, 2);
}

#[test]
fn the_updater_refuses_what_it_should() {
    let (mut f, l, c) = device();
    assert_eq!(
        update(&mut f, &l, &c, &make_image(1, 9000)),
        Err(UpdateError::NotNewer)
    );
    let mut forged = make_image(2, 10000);
    forged[ota::HEADER_LEN as usize + 10] ^= 1;
    assert_eq!(update(&mut f, &l, &c, &forged), Err(UpdateError::BadImage));
    let mut resigned = make_image(2, 10000);
    resigned[4] = 9; // version field, covered by the signature
    assert_eq!(
        update(&mut f, &l, &c, &resigned),
        Err(UpdateError::BadImage)
    );
    let too_big = make_image(2, (l.max_body() + 1) as usize);
    assert_eq!(update(&mut f, &l, &c, &too_big), Err(UpdateError::BadImage));
    update(&mut f, &l, &c, &make_image(2, 10000)).unwrap();
    assert_eq!(
        update(&mut f, &l, &c, &make_image(3, 11000)),
        Err(UpdateError::TrialPending)
    );
}

#[test]
fn an_image_signed_by_another_key_is_refused() {
    use ed25519_dalek::{Signer, SigningKey};
    let (mut f, l, c) = device();
    let mut img = make_image(2, 10000);
    let other = SigningKey::from_bytes(&[8u8; 32]);
    let sig = other.sign(&img[..ota::SIGNED_LEN]);
    img[ota::SIGNED_LEN..ota::SIGNED_LEN + 64].copy_from_slice(&sig.to_bytes());
    assert_eq!(update(&mut f, &l, &c, &img), Err(UpdateError::BadImage));
}

#[test]
fn an_update_never_touches_the_confirmed_slot() {
    let (mut f, l, c) = device();
    let a = l.slots[0] as usize..(l.slots[0] + l.slot_size) as usize;
    let before = f.mem[a.clone()].to_vec();
    update(&mut f, &l, &c, &make_image(2, 10000)).unwrap();
    boot(&mut f, &l, &c);
    assert_eq!(&f.mem[a], &before[..]);
}

#[test]
fn metadata_alternates_sectors_with_rising_sequence_numbers() {
    let (mut f, l, c) = device();
    update(&mut f, &l, &c, &make_image(2, 10000)).unwrap();
    let mut prev = ota::read_meta(&mut f, &l, &c).unwrap().unwrap();
    // Each trial boot spends an attempt: one record per boot.
    for _ in 0..3 {
        boot(&mut f, &l, &c);
        let now = ota::read_meta(&mut f, &l, &c).unwrap().unwrap();
        assert_eq!(now.0.seq, prev.0.seq + 1);
        assert_ne!(now.1, prev.1, "the new record goes in the other sector");
        prev = now;
    }
}

#[test]
fn verify_slot_notices_any_flipped_bit() {
    let (mut f, l, _) = device();
    let base = l.slots[0] as usize;
    let len = ota::HEADER_LEN as usize + 9000;
    // Every bit of the signed header, and a spread of bits in the body.
    let bits =
        (0..(ota::SIGNED_LEN + 64) * 8).chain((ota::HEADER_LEN as usize * 8..len * 8).step_by(97));
    for bit in bits {
        let mut g = f.clone();
        g.mem[base + bit / 8] ^= 1 << (bit % 8);
        assert_eq!(
            ota::verify_slot(&mut g, &l, Slot::A, &verifying_key()).unwrap(),
            None,
            "bit {bit}"
        );
    }
    assert!(ota::verify_slot(&mut f, &l, Slot::A, &verifying_key())
        .unwrap()
        .is_some());
}

#[test]
fn erased_metadata_bricks_rather_than_guessing() {
    let (mut f, l, c) = device();
    f.erase_sector(l.meta[0]).unwrap();
    assert_eq!(boot(&mut f, &l, &c), Boot::Brick(Brick::NoMetadata));
}
