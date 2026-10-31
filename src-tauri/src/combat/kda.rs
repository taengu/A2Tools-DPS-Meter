//! Your kills, assists and deaths against other players, counted.
//!
//! The record that names who killed a player (`04 8D`) comes for every
//! player who dies in range, not only for you. When it names you, that is a
//! kill. When it names someone else and you hit the player a moment before,
//! that is an assist. Your own deaths come from the death recap.
//!
//! Only the kind and the time leave this module: no names, no one else's
//! data. Plugins get them with the `events:kda` permission.

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Kill,
    Assist,
    Death,
}

/// One kill, assist or death, as plugins get it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct KdaEvent {
    /// When (unix ms).
    pub at: i64,
    pub kind: Kind,
}

#[derive(Default)]
pub struct Kda {
    /// Your last hit on each target (time).
    my_hits: HashMap<i32, i64>,
    /// Players already counted as dead, and when.
    told: HashMap<i32, i64>,
    events: VecDeque<KdaEvent>,
    seq: u64,
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

    /// Player `victim` died at `at`; `by_me` when the record names you.
    /// Returns what it counts as, if anything.
    pub fn player_died(&mut self, victim: i32, by_me: bool, at: i64) -> Option<Kind> {
        if self.told.get(&victim).is_some_and(|when| (at - when).abs() < SAME_DEATH_MS) {
            return None;
        }
        let kind = if by_me {
            Kind::Kill
        } else if self.my_hits.get(&victim).is_some_and(|last| at - last <= ASSIST_MS && at >= last - 1_000) {
            Kind::Assist
        } else {
            return None;
        };
        self.told.insert(victim, at);
        self.my_hits.remove(&victim);
        if self.told.len() > MAX_TARGETS {
            self.told.retain(|_, when| *when >= at - SAME_DEATH_MS);
        }
        self.push(at, kind);
        Some(kind)
    }

    /// A player killed you (the death recap has already left out repeats).
    pub fn died(&mut self, at: i64) {
        self.push(at, Kind::Death);
    }

    fn push(&mut self, at: i64, kind: Kind) {
        self.events.push_back(KdaEvent { at, kind });
        while self.events.len() > MAX_EVENTS {
            self.events.pop_front();
        }
        self.seq += 1;
    }

    /// The events made since `seq` was read, and the count to ask with next
    /// time; None when there are none.
    pub fn since(&self, seq: u64) -> Option<(u64, Vec<KdaEvent>)> {
        if self.seq == seq {
            return None;
        }
        let new = (self.seq - seq).min(self.events.len() as u64) as usize;
        Some((self.seq, self.events.iter().skip(self.events.len() - new).copied().collect()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_record_naming_you_is_a_kill() {
        let mut kda = Kda::default();
        assert_eq!(kda.player_died(7, true, 1_000), Some(Kind::Kill));
        let (seq, events) = kda.since(0).unwrap();
        assert_eq!(seq, 1);
        assert_eq!(events, vec![KdaEvent { at: 1_000, kind: Kind::Kill }]);
        assert!(kda.since(seq).is_none());
    }

    #[test]
    fn a_player_you_hit_just_before_another_killed_them_is_an_assist() {
        let mut kda = Kda::default();
        kda.note_my_hit(7, 1_000);
        kda.note_my_hit(7, 1_400);
        assert_eq!(kda.player_died(7, false, 1_800), Some(Kind::Assist));
    }

    #[test]
    fn a_player_you_never_hit_counts_for_nothing() {
        let mut kda = Kda::default();
        kda.note_my_hit(8, 1_000);
        assert_eq!(kda.player_died(7, false, 1_800), None);
        assert!(kda.since(0).is_none());
    }

    #[test]
    fn an_old_hit_is_no_assist() {
        let mut kda = Kda::default();
        kda.note_my_hit(7, 1_000);
        assert_eq!(kda.player_died(7, false, 1_000 + ASSIST_MS + 1), None);
    }

    #[test]
    fn the_same_death_read_again_counts_once() {
        let mut kda = Kda::default();
        assert_eq!(kda.player_died(7, true, 1_000), Some(Kind::Kill));
        assert_eq!(kda.player_died(7, true, 1_050), None);
        // Killed again later is another kill.
        assert_eq!(kda.player_died(7, true, 1_000 + SAME_DEATH_MS + 1), Some(Kind::Kill));
        assert_eq!(kda.since(0).unwrap().1.len(), 2);
    }

    #[test]
    fn your_deaths_are_told_with_the_rest_in_order() {
        let mut kda = Kda::default();
        kda.player_died(7, true, 1_000);
        kda.died(2_000);
        let (seq, events) = kda.since(1).unwrap();
        assert_eq!(seq, 2);
        assert_eq!(events, vec![KdaEvent { at: 2_000, kind: Kind::Death }]);
    }
}
