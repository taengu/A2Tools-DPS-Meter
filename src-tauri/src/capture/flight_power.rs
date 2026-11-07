//! Flight power (FP): how much is left, the maximum, and how fast it refills,
//! for the meter's FP timer. Read 2026-11-07 from the user's capture (flying,
//! refilling, a Superior Wind Serum) and their login capture.
//!
//! ```text
//! 00 8D <entity varint> <head u8> <count u8> [<resource u8> <value u32>] x count [tail]
//! ```
//!
//! The resource is the game's EResource plus one: HP 1, MP 2, SP 3, DP 4,
//! OP 5, FP 6, AP 7, then their maximums HP_Max 8 ... FP_Max 13, AP_Max 14.
//! Values are in hundredths (ResourceStatUnit 100): FP_Max 629807 was the
//! 6298 Flight Power of the user's stat window. Head 1 is a change, sent
//! once a second while FP refills (+4000, 40 FP/s) and every ~0.1 s while
//! flying (-1000); head 3 the whole sheet, seen at login, with a tail after
//! the entries. FP_Max came only in that sheet. A Superior Wind Serum
//! (FpHeal_Item 200000) showed as one jump of +199000 while flying.

use std::collections::HashMap;

use serde::Serialize;

use super::stream_processor::read_varint;

const FP: u8 = 6;
const FP_MAX: u8 = 13;
/// What FP refilled by each second, in the captures (hundredths).
const DEFAULT_RATE: u32 = 4000;
/// What flying drained each second: 1000 every ~0.1 s.
const DEFAULT_DRAIN: u32 = 10_000;

/// What one `00 8D` says about an entity's flight power.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reading {
    pub entity: i32,
    pub current: Option<u32>,
    pub max: Option<u32>,
}

/// A `00 8D` with FP or its maximum in it.
pub fn parse(packet: &[u8]) -> Option<Reading> {
    let len = read_varint(packet, 0);
    if len.length <= 0 {
        return None;
    }
    let at = len.length as usize;
    if packet.get(at..at + 2)? != [0x00, 0x8D] {
        return None;
    }
    let entity = read_varint(packet, at + 2);
    if entity.length <= 0 || !(1..=9_999_999).contains(&entity.value) {
        return None;
    }
    let mut i = at + 2 + entity.length as usize;
    let (head, count) = (*packet.get(i)?, *packet.get(i + 1)? as usize);
    if !(1..=3).contains(&head) || count == 0 || count > 14 {
        return None;
    }
    i += 2;
    let mut reading = Reading { entity: entity.value, current: None, max: None };
    for _ in 0..count {
        let resource = *packet.get(i)?;
        let value = u32::from_le_bytes(packet.get(i + 1..i + 5)?.try_into().ok()?);
        if !(1..=14).contains(&resource) {
            return None;
        }
        match resource {
            FP => reading.current = Some(value),
            FP_MAX => reading.max = Some(value),
            _ => {}
        }
        i += 5;
    }
    // A change ends with its entries; only the whole sheet has more after.
    if head == 1 && i != packet.len() {
        return None;
    }
    (reading.current.is_some() || reading.max.is_some()).then_some(reading)
}

/// The FP timer's figures, in hundredths, as the page gets them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FlightPower {
    pub current: u32,
    /// Unknown until the game sends its whole sheet (at login).
    pub max: Option<u32>,
    /// What it refills by each second.
    pub rate_per_s: u32,
    /// Falling at the last reading: in flight.
    pub flying: bool,
    /// What flying drains each second.
    pub drain_per_s: u32,
    /// When `current` was read (ms).
    pub at_ms: i64,
}

#[derive(Default)]
struct Entity {
    current: Option<(u32, i64)>,
    max: Option<u32>,
    rate: Option<u32>,
    drain: Option<u32>,
    flying: bool,
}

/// Each entity's flight power as read (the game sends it for you only).
#[derive(Default)]
pub struct FlightPowers {
    entities: HashMap<i32, Entity>,
}

impl FlightPowers {
    pub fn note(&mut self, reading: Reading, at_ms: i64) {
        if self.entities.len() > 16 && !self.entities.contains_key(&reading.entity) {
            self.entities.clear();
        }
        let e = self.entities.entry(reading.entity).or_default();
        if let Some(max) = reading.max {
            e.max = Some(max);
        }
        if let Some(current) = reading.current {
            // The refill rate, from a rise one tick (about a second) after
            // the last reading: a serum's jump, or a reading ~0.1 s apart in
            // flight, says nothing about it.
            if let Some((was, was_at)) = e.current {
                let step = current.saturating_sub(was);
                let dt = at_ms - was_at;
                if current > was && (700..=1500).contains(&dt) && step <= 50_000 {
                    e.rate = Some(step);
                }
                // In flight: the drain from a ~0.1 s step, smoothed; a
                // take-off's own cost (2500, 3500 at once) left out.
                e.flying = current < was;
                let fall = was.saturating_sub(current);
                if current < was && fall <= 2_000 && (50..=400).contains(&dt) {
                    let per_s = (fall as i64 * 1000 / dt) as u32;
                    e.drain = Some(match e.drain {
                        Some(d) => (d * 7 + per_s) / 8,
                        None => per_s,
                    });
                }
            }
            e.current = Some((current, at_ms));
        }
    }

    pub fn of(&self, entity: i32) -> Option<FlightPower> {
        let e = self.entities.get(&entity)?;
        let (current, at_ms) = e.current?;
        Some(FlightPower {
            current,
            max: e.max,
            rate_per_s: e.rate.unwrap_or(DEFAULT_RATE),
            flying: e.flying,
            drain_per_s: e.drain.unwrap_or(DEFAULT_DRAIN),
            at_ms,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }

    /// From the user's captures: a refill reading, the serum's two-entry
    /// change and the login sheet with the maximum.
    #[test]
    fn flight_power_and_its_maximum() {
        assert_eq!(
            parse(&hex("0F008D95790101061ABA0300")),
            Some(Reading { entity: 15509, current: Some(244_250), max: None })
        );
        // Two at once: SP 3, FP 6.
        assert_eq!(
            parse(&hex("14008D9579010203C0D4010006B2710400")),
            Some(Reading { entity: 15509, current: Some(291_250), max: None })
        );
        // The login sheet: HP_Max, SP_Max, DP_Max, OP_Max and FP_Max 629807,
        // with its tail.
        let sheet = hex("2D008DDD6C030508E90800000AB4D601000BF04902000CA08601000D2F9C09000107D132000000000000");
        assert_eq!(parse(&sheet), Some(Reading { entity: 13917, current: None, max: Some(629_807) }));
        assert_eq!(parse(&hex("19008DDD6C030108680A000001070F37000000000000")), None, "HP_Max only");
        // HP only: nothing for the timer.
        assert_eq!(parse(&hex("0F008D9579010101E8030000")), None);
        // Not a resource packet.
        assert_eq!(parse(&hex("0E0036E27E99B6A1010000")), None);
    }

    #[test]
    fn the_refill_rate_comes_from_a_second_s_rise() {
        let mut fp = FlightPowers::default();
        let read = |current: u32| Reading { entity: 7, current: Some(current), max: None };
        fp.note(read(100_000), 0);
        assert_eq!(fp.of(7).map(|f| (f.rate_per_s, f.max)), Some((DEFAULT_RATE, None)));
        fp.note(read(104_500), 1_000);
        assert_eq!(fp.of(7).unwrap().rate_per_s, 4_500);
        // A serum: no rate from it.
        fp.note(read(304_500), 2_000);
        assert_eq!(fp.of(7).unwrap().rate_per_s, 4_500);
        fp.note(Reading { entity: 7, current: None, max: Some(629_807) }, 2_100);
        assert_eq!(
            fp.of(7),
            Some(FlightPower { current: 304_500, max: Some(629_807), rate_per_s: 4_500, flying: false, drain_per_s: DEFAULT_DRAIN, at_ms: 2_000 })
        );
        // Take-off (3500 at once), then flying: 1000 a tenth of a second.
        fp.note(read(301_000), 2_900);
        for (k, at) in (3_000..3_600).step_by(100).enumerate() {
            fp.note(read(300_000 - 1_000 * k as u32), at);
        }
        let f = fp.of(7).unwrap();
        assert!(f.flying);
        assert!((9_000..=11_000).contains(&f.drain_per_s), "{}", f.drain_per_s);
        // Landed: refilling again.
        fp.note(read(299_000), 4_600);
        assert!(!fp.of(7).unwrap().flying);
    }
}
