//! Your kills, assists and deaths against other players, counted.
//!
//! The record that names who killed a player (`04 8D`) comes for every
//! player who dies in range, not only for you. When it names you, that is a
//! kill. When it names someone on your side and you hit the player a moment
//! before, that is an assist: your "hits" take in buffs on players near you,
//! so an ally an enemy killed is no assist (the caller decides the sides).
//! Your own deaths come from the death recap.
//!
//! The Abyss comes with them: the Abyss Points you gain (`points`, with the
//! `amount`, the week's total so far, `week`, and its cap, `limit`) and
//! entering or leaving the Abyss (`zone`, with `abyss`).
//!
//! A kill or assist names the player killed (`Victim`: name, server, class,
//! legion, as their record gave them), for the plugin's history of them.
//! Plugins get them with the `events:kda` permission.

use std::collections::{HashMap, VecDeque};

use serde::Serialize;

/// A hit of yours counts toward an assist this long.
const ASSIST_MS: i64 = 15_000;
/// The same death, as one record read again.
const SAME_DEATH_MS: i64 = 10_000;
/// Events kept for plugins whose window opens later.
const MAX_EVENTS: usize = 50;
/// Targets' last hits kept.
const MAX_TARGETS: usize = 512;
/// The most Abyss Points one update can be told as earned. A bigger jump is
/// another character's total at a login, not a gain.
const MAX_GAIN: u64 = 20_000;
/// The maps of the Abyss: the first layer (seen in a 2026-11-02 capture) and
/// the second (from the artifact ids, not seen loaded).
const ABYSS_MAPS: [i32; 2] = [20, 22];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Kill,
    Assist,
    Death,
    Points,
    Zone,
}

/// The player a kill or assist was on, as their record gave them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Victim {
    pub name: Option<String>,
    pub server_id: Option<u16>,
    /// "gladiator", "templar", ...
    pub class: Option<&'static str>,
    pub level: Option<u32>,
    pub legion: Option<String>,
}

/// One kill, assist or death, as plugins get it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct KdaEvent {
    /// When (unix ms).
    pub at: i64,
    /// The event's number, counting from 1: two in the same millisecond
    /// differ by it.
    pub seq: u64,
    pub kind: Kind,
    /// Abyss Points gained (`Points`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub amount: Option<u64>,
    /// Whether the map is the Abyss (`Zone`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub abyss: Option<bool>,
    /// The week's Abyss Points so far and their cap (`Points`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub week: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<u64>,
    /// Who was killed (`Kill`, `Assist`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub victim: Option<Victim>,
}

/// An event of `kind` at `at` with nothing more to it.
fn event(at: i64, kind: Kind) -> KdaEvent {
    KdaEvent { at, seq: 0, kind, amount: None, abyss: None, week: None, limit: None, victim: None }
}

#[derive(Default)]
pub struct Kda {
    /// Your last hit on each target (time).
    my_hits: HashMap<i32, i64>,
    /// Players already counted as dead, and when.
    told: HashMap<i32, i64>,
    events: VecDeque<KdaEvent>,
    seq: u64,
    /// The Abyss Points total and cap as last sent.
    points: Option<(u64, u64)>,
    /// Whether the last map loaded was the Abyss.
    in_abyss: Option<bool>,
}

impl Kda {
    /// A hit of yours on `target`.
    pub fn note_my_hit(&mut self, target: i32, at: i64) {
        self.my_hits.insert(target, at);
        if self.my_hits.len() > MAX_TARGETS {
            self.my_hits.retain(|_, last| *last >= at - ASSIST_MS);
            if self.my_hits.len() > MAX_TARGETS {
                self.my_hits.clear();
            }
        }
    }

    /// How long before `at` your last hit on `target` was (ms), if you hit it.
    pub fn hit_age(&self, target: i32, at: i64) -> Option<i64> {
        self.my_hits.get(&target).map(|last| at - last)
    }

    /// Player `victim` died at `at`; `by_me` when the record names you,
    /// `ally_killer` when it names someone on your side. Returns what it
    /// counts as, if anything; `who` goes with it.
    pub fn player_died(&mut self, victim: i32, by_me: bool, ally_killer: bool, at: i64, who: Victim) -> Option<Kind> {
        if self.told.get(&victim).is_some_and(|when| (at - when).abs() < SAME_DEATH_MS) {
            return None;
        }
        let kind = if by_me {
            Kind::Kill
        } else if ally_killer && self.my_hits.get(&victim).is_some_and(|last| at - last <= ASSIST_MS && at >= last - 1_000) {
            Kind::Assist
        } else {
            return None;
        };
        self.told.insert(victim, at);
        self.my_hits.remove(&victim);
        if self.told.len() > MAX_TARGETS {
            self.told.retain(|_, when| *when >= at - SAME_DEATH_MS);
        }
        self.push_event(KdaEvent { at, seq: 0, kind, amount: None, abyss: None, week: None, limit: None, victim: Some(who) });
        Some(kind)
    }

    /// A player killed you (the death recap has already left out repeats).
    pub fn died(&mut self, at: i64) {
        self.push_event(KdaEvent { at, seq: 0, kind: Kind::Death, amount: None, abyss: None, week: None, limit: None, victim: None });
    }

    /// The week's Abyss Points total and cap the game just sent: told when
    /// either changed, a rise as earned (`amount`). The first, at a login,
    /// and a fall (the weekly reset) only give the week.
    pub fn abyss_points(&mut self, total: u64, limit: u64, at: i64) {
        let before = self.points.replace((total, limit));
        if before == Some((total, limit)) {
            return;
        }
        let amount = before.map(|(b, _)| b).filter(|b| total > *b && total - b <= MAX_GAIN).map(|b| total - b);
        self.push_event(KdaEvent { amount, week: Some(total), limit: Some(limit), ..event(at, Kind::Points) });
    }

    /// A map loaded: told when it is the Abyss and the last was not, or the
    /// other way round.
    pub fn map_loaded(&mut self, map_id: i32, at: i64) {
        let abyss = ABYSS_MAPS.contains(&map_id);
        let before = self.in_abyss.replace(abyss);
        if before != Some(abyss) && (abyss || before.is_some()) {
            self.push_event(KdaEvent { at, seq: 0, kind: Kind::Zone, amount: None, abyss: Some(abyss), week: None, limit: None, victim: None });
        }
    }

    fn push_event(&mut self, mut event: KdaEvent) {
        self.seq += 1;
        event.seq = self.seq;
        self.events.push_back(event);
        while self.events.len() > MAX_EVENTS {
            self.events.pop_front();
        }
    }

    /// The events made since `seq` was read, and the count to ask with next
    /// time; None when there are none.
    pub fn since(&self, seq: u64) -> Option<(u64, Vec<KdaEvent>)> {
        if self.seq == seq {
            return None;
        }
        let new = (self.seq - seq).min(self.events.len() as u64) as usize;
        Some((self.seq, self.events.iter().skip(self.events.len() - new).cloned().collect()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn died(kda: &mut Kda, victim: i32, by_me: bool, at: i64) -> Option<Kind> {
        kda.player_died(victim, by_me, true, at, Victim::default())
    }

    #[test]
    fn a_record_naming_you_is_a_kill() {
        let mut kda = Kda::default();
        let who = Victim { name: Some("Enemy".into()), server_id: Some(2004), class: Some("ranger"), level: Some(45), legion: None };
        assert_eq!(kda.player_died(7, true, false, 1_000, who.clone()), Some(Kind::Kill));
        let (seq, events) = kda.since(0).unwrap();
        assert_eq!(seq, 1);
        assert_eq!(events, vec![KdaEvent { at: 1_000, seq: 1, kind: Kind::Kill, amount: None, abyss: None, week: None, limit: None, victim: Some(who) }]);
        assert!(kda.since(seq).is_none());
    }

    #[test]
    fn a_player_you_hit_just_before_another_killed_them_is_an_assist() {
        let mut kda = Kda::default();
        kda.note_my_hit(7, 1_000);
        kda.note_my_hit(7, 1_400);
        assert_eq!(died(&mut kda, 7, false, 1_800), Some(Kind::Assist));
    }

    /// Your buff on a player near you, then an enemy killed them: their
    /// death, not your assist.
    #[test]
    fn an_ally_an_enemy_killed_is_no_assist() {
        let mut kda = Kda::default();
        kda.note_my_hit(7, 1_000);
        assert_eq!(kda.player_died(7, false, false, 1_800, Victim::default()), None);
        assert!(kda.since(0).is_none());
    }

    #[test]
    fn a_player_you_never_hit_counts_for_nothing() {
        let mut kda = Kda::default();
        kda.note_my_hit(8, 1_000);
        assert_eq!(died(&mut kda, 7, false, 1_800), None);
        assert!(kda.since(0).is_none());
    }

    #[test]
    fn an_old_hit_is_no_assist() {
        let mut kda = Kda::default();
        kda.note_my_hit(7, 1_000);
        assert_eq!(died(&mut kda, 7, false, 1_000 + ASSIST_MS + 1), None);
    }

    #[test]
    fn the_same_death_read_again_counts_once() {
        let mut kda = Kda::default();
        assert_eq!(died(&mut kda, 7, true, 1_000), Some(Kind::Kill));
        assert_eq!(died(&mut kda, 7, true, 1_050), None);
        // Killed again later is another kill.
        assert_eq!(died(&mut kda, 7, true, 1_000 + SAME_DEATH_MS + 1), Some(Kind::Kill));
        assert_eq!(kda.since(0).unwrap().1.len(), 2);
    }

    #[test]
    fn your_deaths_are_told_with_the_rest_in_order() {
        let mut kda = Kda::default();
        died(&mut kda, 7, true, 1_000);
        kda.died(2_000);
        let (seq, events) = kda.since(1).unwrap();
        assert_eq!(seq, 2);
        assert_eq!(events, vec![KdaEvent { at: 2_000, seq: 2, kind: Kind::Death, amount: None, abyss: None, week: None, limit: None, victim: None }]);
    }

    #[test]
    fn a_rise_in_the_abyss_points_is_told_as_earned() {
        let mut kda = Kda::default();
        // The first total, at a login, gives the week; the same again is nothing.
        kda.abyss_points(132_456, 3_500_000, 1_000);
        kda.abyss_points(132_456, 3_500_000, 2_000);
        let (seq, events) = kda.since(0).unwrap();
        assert_eq!(events.iter().map(|e| (e.amount, e.week, e.limit)).collect::<Vec<_>>(), vec![(None, Some(132_456), Some(3_500_000))]);
        kda.abyss_points(133_369, 3_500_000, 3_000);
        let (_, events) = kda.since(seq).unwrap();
        assert_eq!(events, vec![KdaEvent { amount: Some(913), week: Some(133_369), limit: Some(3_500_000), seq: 2, ..event(3_000, Kind::Points) }]);
    }

    #[test]
    fn a_lower_or_far_higher_total_is_not_a_gain() {
        let mut kda = Kda::default();
        kda.abyss_points(500, 3_500_000, 1_000);
        kda.abyss_points(100, 3_500_000, 2_000);
        kda.abyss_points(100 + MAX_GAIN + 1, 3_500_000, 3_000);
        kda.abyss_points(100 + MAX_GAIN + 11, 3_500_000, 4_000);
        let amounts: Vec<_> = kda.since(0).unwrap().1.iter().map(|e| e.amount).collect();
        assert_eq!(amounts, vec![None, None, None, Some(10)]);
    }

    #[test]
    fn entering_and_leaving_the_abyss_is_told_once() {
        let mut kda = Kda::default();
        kda.map_loaded(1010, 1_000);
        assert!(kda.since(0).is_none());
        kda.map_loaded(20, 2_000);
        kda.map_loaded(22, 3_000);
        kda.map_loaded(1010, 4_000);
        let (_, events) = kda.since(0).unwrap();
        let told: Vec<_> = events.iter().map(|e| (e.at, e.abyss)).collect();
        assert_eq!(told, vec![(2_000, Some(true)), (4_000, Some(false))]);
    }

    #[test]
    fn a_meter_started_in_the_abyss_hears_it_at_the_first_load() {
        let mut kda = Kda::default();
        kda.map_loaded(20, 1_000);
        assert_eq!(kda.since(0).unwrap().1[0].abyss, Some(true));
    }

    #[test]
    fn two_kills_in_the_same_millisecond_are_two_events() {
        let mut kda = Kda::default();
        died(&mut kda, 7, true, 1_000);
        died(&mut kda, 8, true, 1_000);
        let events = kda.since(0).unwrap().1;
        assert_eq!(events.iter().map(|e| (e.at, e.seq)).collect::<Vec<_>>(), vec![(1_000, 1), (1_000, 2)]);
    }
}
