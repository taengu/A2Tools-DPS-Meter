//! Who killed you, put together once the game says so.
//!
//! The kill record (`04 8D`) that names you as the one killed names the
//! player who did it. The recap is then their profile, as their own record
//! (`45 36`) gave it, and what they did to you in that fight.
//!
//! Everyone else who damaged you in that fight is in it too, as an assist
//! (your party never can).
//!
//! What it is made from stays here until then: a recap is the only thing
//! that leaves, and only after a death. Plugins get it with the
//! `events:death-recap` permission.

use std::collections::{HashMap, VecDeque};

use serde::Serialize;

use crate::entity::job_class::JobClass;

/// Players' profiles kept, the one seen longest ago dropped first. 256 lasted
/// seconds in a big fight: 1,625 players in the two minutes before one death,
/// and the killer's was gone by then (report 28KXWE, 2026-11-01). Each is a
/// few dozen bytes.
const MAX_PROFILES: usize = 4096;
/// Hits on you older than this are dropped.
const HIT_WINDOW_MS: i64 = 60_000;
/// A gap longer than this between two of the killer's hits starts the fight
/// over.
const FIGHT_GAP_MS: i64 = 15_000;
/// The same death, as one record read again (the record scan sees a buffer
/// more than once) or sent twice.
const SAME_DEATH_MS: i64 = 10_000;
/// Recaps kept for plugins whose window opens later.
const MAX_RECAPS: usize = 20;
/// Assists in one recap, the most damage first.
const MAX_ASSISTS: usize = 10;

/// What a player's record says of them. Each part is `None` when the record
/// did not give it plainly.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Profile {
    /// Set by the caller, from the record's name field.
    pub name: Option<String>,
    pub class: Option<JobClass>,
    pub level: Option<u32>,
    pub gear_score: Option<u32>,
    pub combat_power: Option<u64>,
    /// Their legion's name and server, when they are in one.
    pub legion: Option<String>,
    pub server_id: Option<u16>,
}

/// A player's profile from their record. `rest` is the record from just past
/// the name to the end of its frame.
///
/// - The class is the u32 right after the name, in the party roster's values.
/// - The level and gear score sit as `48 f4 ff ff <level u32> 00 00 00 00
///   <gear score u32>`.
/// - The legion, in one: `<legion id u16> 00 00 00 00 <server u16> <len u8>
///   <utf8 name>`, about 100 bytes on (DISSONATA on 2304 for the same
///   Gladiator, as the kill record named it; 101 of 225 players had one).
/// - The combat power is near the end: `.. f4 cb 1f <u32> <n u32>`, maybe
///   one or four bytes, then the combat power (u64) and one or two last
///   bytes.
///
/// Read on 2026-10-28 from 512 records of 354 players: a Gladiator who
/// killed the player read level 45, gear score 2783 and combat power 170,738,
/// as two sites listed him (2783 or 2786, about 170.7k); each player seen
/// more than once read a combat power and gear score that only grew.
pub fn profile(rest: &[u8]) -> Profile {
    let u32_at = |at: usize| rest.get(at..at + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap()));
    let class = u32_at(0).and_then(JobClass::from_roster_class);
    let legion = legion(rest);
    let level_gs = (4..rest.len().saturating_sub(12)).find_map(|i| {
        if rest[i - 4..i] != [0x48, 0xf4, 0xff, 0xff] || rest[i + 4..i + 8] != [0, 0, 0, 0] {
            return None;
        }
        let (level, gs) = (u32_at(i)?, u32_at(i + 8)?);
        ((1..=99).contains(&level) && (1..=10_000).contains(&gs)).then_some((level, gs))
    });
    Profile {
        name: None,
        class,
        level: level_gs.map(|(l, _)| l),
        gear_score: level_gs.map(|(_, g)| g),
        combat_power: combat_power(rest),
        legion: legion.as_ref().map(|(name, _)| name.clone()),
        server_id: legion.map(|(_, server)| server),
    }
}

/// The legion's name and server: the first place in the record's first 200
/// bytes that reads as one.
fn legion(rest: &[u8]) -> Option<(String, u16)> {
    let head = &rest[..rest.len().min(200)];
    (0..head.len().saturating_sub(10)).find_map(|i| {
        if head[i..i + 2] == [0, 0] || head[i + 2..i + 6] != [0, 0, 0, 0] {
            return None;
        }
        let server = u16::from_le_bytes([head[i + 6], head[i + 7]]);
        let len = head[i + 8] as usize;
        if !crate::capture::servers::exists(server) || !(1..=24).contains(&len) {
            return None;
        }
        let name = std::str::from_utf8(head.get(i + 9..i + 9 + len)?).ok()?;
        name.chars().all(|c| c.is_alphanumeric() || c == ' ').then(|| (name.to_string(), server))
    })
}

fn combat_power(rest: &[u8]) -> Option<u64> {
    let from = rest.len().saturating_sub(40);
    let marker = rest[from..].windows(3).rposition(|w| w == [0xf4, 0xcb, 0x1f])?;
    let tail = &rest[from + marker + 3..];
    if tail.len() < 17 || !(1..=0x40).contains(&tail[4]) || tail[5..8] != [0, 0, 0] {
        return None;
    }
    // The fewer bytes before it, the likelier: a combat power read one byte
    // early takes the byte before as its lowest, and comes out hundreds of
    // times too high (170,738 read as 43,708,931).
    [8, 9, 12, 13].into_iter().find_map(|at| {
        let after = tail.len().checked_sub(at + 8)?;
        if !(1..=2).contains(&after) {
            return None;
        }
        let cp = u64::from_le_bytes(tail[at..at + 8].try_into().unwrap());
        (1_000..=3_000_000).contains(&cp).then_some(cp)
    })
}

/// The player who killed you, as the kill record names them.
#[derive(Debug, Clone)]
pub struct Killer {
    pub id: i32,
    pub name: String,
    pub server_id: u16,
    pub legion: Option<String>,
}

#[derive(Debug, Clone, Copy)]
struct Hit {
    at: i64,
    damage: i64,
    skill: i32,
}

/// One death, as plugins get it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeathRecap {
    /// When you died (unix ms).
    pub at: i64,
    pub killer: KillerProfile,
    /// The killer's damage to you in the fight, and in how many hits.
    pub damage: i64,
    pub hits: u32,
    /// From their first hit in the fight to your death; null when they never
    /// hit you that the meter saw.
    pub duration_ms: Option<i64>,
    /// Their damage per second over the fight (a fight under a second counts
    /// as one).
    pub dps: Option<i64>,
    /// Their skills, the most damage first.
    pub skills: Vec<SkillDamage>,
    /// The other players who damaged you in the fight, the most first.
    pub assists: Vec<Assist>,
}

/// A player who damaged you in the fight that killed you, other than the
/// killer. What their record gave; null where it gave nothing.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Assist {
    pub name: Option<String>,
    pub legion: Option<String>,
    pub server_id: Option<u16>,
    pub class: Option<&'static str>,
    pub level: Option<u32>,
    pub gear_score: Option<u32>,
    pub combat_power: Option<u64>,
    pub damage: i64,
    pub hits: u32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KillerProfile {
    pub name: String,
    pub server_id: u16,
    pub legion: Option<String>,
    /// "gladiator", "templar", ...
    pub class: Option<&'static str>,
    pub level: Option<u32>,
    pub gear_score: Option<u32>,
    pub combat_power: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillDamage {
    pub id: i32,
    /// In the meter's language, when known.
    pub name: Option<String>,
    pub damage: i64,
    pub hits: u32,
}

/// One attacker's hits in the fight that ended at `at`: up to the death, back
/// to the first after a long gap; the latest first.
fn fight_hits(hits: Option<VecDeque<Hit>>, at: i64) -> Vec<Hit> {
    let mut fight: Vec<Hit> = Vec::new();
    for hit in hits.map(Vec::from).unwrap_or_default().into_iter().rev().filter(|h| h.at <= at + 1_000) {
        let next = fight.last().map_or(at, |h| h.at);
        if next - hit.at > FIGHT_GAP_MS {
            break;
        }
        fight.push(hit);
    }
    fight
}

fn class_key(class: JobClass) -> &'static str {
    match class {
        JobClass::Gladiator => "gladiator",
        JobClass::Templar => "templar",
        JobClass::Ranger => "ranger",
        JobClass::Assassin => "assassin",
        JobClass::Sorcerer => "sorcerer",
        JobClass::Cleric => "cleric",
        JobClass::Elementalist => "elementalist",
        JobClass::Chanter => "chanter",
        JobClass::Fighter => "fighter",
    }
}

#[derive(Default)]
pub struct DeathRecaps {
    profiles: HashMap<i32, Profile>,
    order: VecDeque<i32>,
    /// Other players' hits on you, by attacker.
    hits: HashMap<i32, VecDeque<Hit>>,
    last_death: Option<(i32, i64)>,
    recaps: VecDeque<DeathRecap>,
    seq: u64,
}

impl DeathRecaps {
    /// A player's record read: what it gives replaces what was known, and
    /// what it doesn't give is kept. A record is read more than once, still
    /// packed as well as decoded, and the packed read gives nothing: it came
    /// last and wiped the killer's class, gear score and combat power (Ess,
    /// report 28KXWE, 2026-11-01).
    pub fn note_profile(&mut self, id: i32, profile: Profile) {
        let profile = match self.profiles.get(&id) {
            Some(known) => Profile {
                name: profile.name.or_else(|| known.name.clone()),
                class: profile.class.or(known.class),
                level: profile.level.or(known.level),
                gear_score: profile.gear_score.or(known.gear_score),
                combat_power: profile.combat_power.or(known.combat_power),
                legion: profile.legion.or_else(|| known.legion.clone()),
                server_id: profile.server_id.or(known.server_id),
            },
            None => profile,
        };
        // Seen again: to the back of the queue, the last to go.
        if self.profiles.insert(id, profile).is_some() {
            self.order.retain(|&x| x != id);
        }
        self.order.push_back(id);
        while self.order.len() > MAX_PROFILES {
            if let Some(old) = self.order.pop_front() {
                self.profiles.remove(&old);
            }
        }
    }

    /// Another player's hit on you.
    pub fn note_hit(&mut self, attacker: i32, at: i64, damage: i64, skill: i32) {
        self.hits.entry(attacker).or_default().push_back(Hit { at, damage, skill });
        self.hits.retain(|_, hits| {
            while hits.front().is_some_and(|h| h.at < at - HIT_WINDOW_MS) {
                hits.pop_front();
            }
            !hits.is_empty()
        });
    }

    /// You were killed by `killer` at `at`: the recap, unless this death was
    /// already told. `party` names your party, whose records are not
    /// assists. Skill names are for the caller to fill in.
    pub fn died(&mut self, killer: Killer, at: i64, party: &[String]) -> Option<DeathRecap> {
        if self.last_death.is_some_and(|(id, when)| id == killer.id && (at - when).abs() < SAME_DEATH_MS) {
            return None;
        }
        self.last_death = Some((killer.id, at));
        let fight = fight_hits(self.hits.remove(&killer.id), at);
        let mut assists: Vec<Assist> = std::mem::take(&mut self.hits)
            .into_iter()
            .filter_map(|(id, hits)| {
                let theirs = fight_hits(Some(hits), at);
                let profile = self.profiles.get(&id).cloned().unwrap_or_default();
                if theirs.is_empty() || profile.name.as_ref().is_some_and(|n| party.contains(n)) {
                    return None;
                }
                Some(Assist {
                    name: profile.name,
                    legion: profile.legion,
                    server_id: profile.server_id,
                    class: profile.class.map(class_key),
                    level: profile.level,
                    gear_score: profile.gear_score,
                    combat_power: profile.combat_power,
                    damage: theirs.iter().map(|h| h.damage).sum(),
                    hits: theirs.len() as u32,
                })
            })
            .filter(|a| a.damage > 0)
            .collect();
        assists.sort_by_key(|a| -a.damage);
        assists.truncate(MAX_ASSISTS);
        let damage: i64 = fight.iter().map(|h| h.damage).sum();
        let duration_ms = fight.last().map(|first| (at - first.at).max(0));
        let mut skills: Vec<SkillDamage> = Vec::new();
        for hit in &fight {
            match skills.iter_mut().find(|s| s.id == hit.skill) {
                Some(s) => {
                    s.damage += hit.damage;
                    s.hits += 1;
                }
                None => skills.push(SkillDamage { id: hit.skill, name: None, damage: hit.damage, hits: 1 }),
            }
        }
        skills.sort_by_key(|s| -s.damage);
        let profile = self.profiles.get(&killer.id).cloned().unwrap_or_default();
        Some(DeathRecap {
            at,
            killer: KillerProfile {
                name: killer.name,
                server_id: killer.server_id,
                legion: killer.legion.or(profile.legion),
                class: profile.class.map(class_key),
                level: profile.level,
                gear_score: profile.gear_score,
                combat_power: profile.combat_power,
            },
            damage,
            hits: fight.len() as u32,
            duration_ms,
            dps: duration_ms.map(|ms| damage * 1000 / ms.max(1000)),
            skills,
            assists,
        })
    }

    pub fn push(&mut self, recap: DeathRecap) {
        self.recaps.push_back(recap);
        while self.recaps.len() > MAX_RECAPS {
            self.recaps.pop_front();
        }
        self.seq += 1;
    }

    /// The recaps made since `seq` was read, and the count to ask with next
    /// time; None when there are none.
    pub fn since(&self, seq: u64) -> Option<(u64, Vec<DeathRecap>)> {
        if self.seq == seq {
            return None;
        }
        let new = (self.seq - seq).min(self.recaps.len() as u64) as usize;
        Some((self.seq, self.recaps.iter().skip(self.recaps.len() - new).cloned().collect()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        s.split_whitespace().map(|b| u8::from_str_radix(b, 16).unwrap()).collect()
    }

    /// A later read that gives nothing keeps what was known, and a player
    /// seen again is the last to be dropped, however many others came since
    /// (report 28KXWE: the killer's profile was gone in a big fight).
    #[test]
    fn a_profile_outlives_empty_reads_and_a_crowd() {
        let mut recaps = DeathRecaps::default();
        let full = Profile { level: Some(45), gear_score: Some(2832), combat_power: Some(160_901), ..Default::default() };
        recaps.note_profile(656, full.clone());
        recaps.note_profile(656, Profile { legion: Some("Heartless".into()), ..Default::default() });
        let kept = &recaps.profiles[&656];
        assert_eq!((kept.gear_score, kept.combat_power, kept.legion.as_deref()), (Some(2832), Some(160_901), Some("Heartless")));
        for id in 1..MAX_PROFILES as i32 {
            recaps.note_profile(10_000 + id, Profile::default());
        }
        recaps.note_profile(656, Profile::default());
        for id in 0..MAX_PROFILES as i32 / 2 {
            recaps.note_profile(20_000 + id, Profile::default());
        }
        assert_eq!(recaps.profiles.get(&656).and_then(|p| p.combat_power), Some(160_901));
        assert!(recaps.profiles.len() <= MAX_PROFILES);
    }

    /// A record's end, past a run of zeros: `<pad> 21 f4 cb 1f <tail>`.
    fn ending(tail: &str) -> Vec<u8> {
        let mut rest = vec![7, 0, 0, 0];
        rest.extend(hex("48 f4 ff ff 2d 00 00 00 00 00 00 00 df 0a 00 00"));
        rest.extend([0; 40]);
        rest.extend(hex("21 f4 cb 1f"));
        rest.extend(hex(tail));
        rest
    }

    #[test]
    fn a_gladiators_record_gives_his_level_gear_score_and_combat_power() {
        // NotYourSnack, 2026-10-28 03:13:16: one byte before the combat power.
        let p = profile(&ending("c0 04 00 00 0c 00 00 00 03 f2 9a 02 00 00 00 00 00 00"));
        assert_eq!(p.class, Some(JobClass::Gladiator));
        assert_eq!((p.level, p.gear_score, p.combat_power), (Some(45), Some(2783), Some(170_738)));
    }

    #[test]
    fn a_record_names_its_players_legion() {
        // NotYourSnack's: legion 132, server 2304, "DISSONATA".
        let mut rest = vec![7, 0, 0, 0, 0x5f, 0x01, 0x00, 0x09];
        rest.extend(hex("84 00 00 00 00 00 00 09 09"));
        rest.extend(b"DISSONATA");
        rest.extend([2, 0, 2, 0, 0, 0]);
        assert_eq!(legion(&rest), Some(("DISSONATA".to_string(), 2304)));
        // None: no legion.
        assert_eq!(legion(&[7, 0, 0, 0, 0xc6, 0x00, 0x18, 0x05, 0x0c, 0x11, 0x11, 0x2b, 0xc4, 0xc9, 0x01]), None);
    }

    #[test]
    fn the_combat_power_is_found_however_many_bytes_come_before_or_after() {
        // Verre: two bytes after it.
        assert_eq!(combat_power(&ending("a0 04 00 00 0c 00 00 00 73 3d 02 00 00 00 00 00 00 00")), Some(146_803));
        // Migueliyu: one after.
        assert_eq!(combat_power(&ending("ee 03 00 00 13 00 00 00 8d 57 01 00 00 00 00 00 00")), Some(87_949));
        // Sobratik: four before.
        assert_eq!(combat_power(&ending("17 04 00 00 13 00 00 00 26 00 00 00 89 a7 00 00 00 00 00 00 00")), Some(42_889));
        // Qiwi: one before, read early it is 13 million.
        assert_eq!(combat_power(&ending("0f 04 00 00 13 00 00 00 01 22 cd 00 00 00 00 00 00 00")), Some(52_514));
        // Cut short: none.
        assert_eq!(combat_power(&ending("0f 04 00 00 13 00 00")), None);
    }

    fn killer() -> Killer {
        Killer { id: 15191, name: "NotYourSnack".into(), server_id: 2304, legion: Some("DISSONATA".into()) }
    }

    #[test]
    fn a_recap_counts_the_killers_hits_in_the_last_fight_only() {
        let mut d = DeathRecaps::default();
        d.note_profile(15191, profile(&ending("c0 04 00 00 0c 00 00 00 03 f2 9a 02 00 00 00 00 00 00")));
        d.note_hit(15191, 1_000, 999, 1);
        // Twenty seconds later, the fight that killed you.
        d.note_hit(15191, 21_000, 4_000, 11_190_340);
        d.note_hit(15191, 21_500, 1_000, 11_200_000);
        d.note_hit(15191, 22_000, 2_000, 11_190_340);
        // Someone else's hit is not theirs: an assist.
        d.note_profile(777, Profile { name: Some("Tewy".into()), ..Profile::default() });
        d.note_hit(777, 22_000, 50_000, 1);
        let r = d.died(killer(), 23_000, &[]).unwrap();
        assert_eq!((r.damage, r.hits, r.duration_ms, r.dps), (7_000, 3, Some(2_000), Some(3_500)));
        assert_eq!(r.skills.iter().map(|s| (s.id, s.damage, s.hits)).collect::<Vec<_>>(), [(11_190_340, 6_000, 2), (11_200_000, 1_000, 1)]);
        assert_eq!((r.killer.class, r.killer.gear_score, r.killer.combat_power), (Some("gladiator"), Some(2783), Some(170_738)));
        assert_eq!(r.assists.iter().map(|a| (a.name.as_deref(), a.damage, a.hits)).collect::<Vec<_>>(), [(Some("Tewy"), 50_000, 1)]);
        // The same record read again is the same death.
        assert!(d.died(killer(), 23_050, &[]).is_none());
    }

    #[test]
    fn your_party_and_old_hits_are_no_assists() {
        let mut d = DeathRecaps::default();
        d.note_profile(5, Profile { name: Some("Healer".into()), ..Profile::default() });
        d.note_hit(5, 9_000, 3_000, 1);
        // Forty seconds before: another fight.
        d.note_hit(6, -30_000, 3_000, 1);
        d.note_hit(7, 9_500, 1_200, 1);
        let r = d.died(killer(), 10_000, &["Healer".to_string()]).unwrap();
        assert_eq!(r.assists.iter().map(|a| (a.name.as_deref(), a.damage)).collect::<Vec<_>>(), [(None, 1_200)]);
    }

    #[test]
    fn a_killer_never_seen_hitting_gives_no_fight_time() {
        let mut d = DeathRecaps::default();
        let r = d.died(killer(), 5_000, &[]).unwrap();
        assert_eq!((r.damage, r.duration_ms, r.dps, r.killer.class), (0, None, None, None));
    }

    #[test]
    fn plugins_get_each_recap_once() {
        let mut d = DeathRecaps::default();
        assert!(d.since(0).is_none());
        let r = d.died(killer(), 5_000, &[]).unwrap();
        d.push(r);
        let (seq, new) = d.since(0).unwrap();
        assert_eq!(new.len(), 1);
        assert!(d.since(seq).is_none());
    }
}
