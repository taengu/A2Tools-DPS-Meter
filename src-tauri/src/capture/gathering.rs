//! Gathering skills, and how each gather goes, for the shared gathering stats
//! (`gather_report`). Read 2026-11-17 on Global with the player labeling
//! every gather and resetting their skills.
//!
//! ```text
//! 37 8D <n u8> n x ( <skill u32> <level u32> ) <u32>   the learned skills, at login
//! 3D 8D 00 00 <points u16> <special points u16>          skills reset (all to 0)
//! 39 8D 00 00 <skill u32> <level u32> <points> <special> a skill learned (level 1)
//! 3B 8D 00 00 <skill u32> <level u32> <points> <special> a skill's level raised
//! ```
//!
//! Each change comes again with `06 3a` or `0e 3a` in place of `00 00`: those
//! are left alone. Skill ids are the GatherSkill table's: 100 Delicate Touch,
//! 300 Lady Luck, 800 Proficient Handling, then per kind of node (1 Ore,
//! 2 Gemstone, 3 Wood, 5 Herb, 6 Source, 7 Odyle) x0800 Seeker, x1000
//! Plentiful Harvest, x1200 Appraiser.
//!
//! The gather start (`31 8D`, `loot::gather` reads its node) decides the
//! whole gather:
//!
//! ```text
//! 31 8D 00 00 <node varint> <n u32> <flags u8> 00 00 <grade u32> <k u8>
//!   k x ( <kind u8> [a stray byte, now and then] <m u8>
//!         m x ( <skill u32> 01 <skill level id u32> ) <4 x u32 gauge values> )
//! ```
//!
//! - flags: 0x02 it comes out upgraded (the result's status 1); 0x04 it was
//!   upgraded as it started. Upgraded but not at the start: the Appraiser
//!   did it (265 gathers of 265). 0x08 << step marks the step of that, up to
//!   the fifth only, so it isn't read.
//! - Each record is a hit on the gauge. Kind 1 is a crit that fills the
//!   gauge at once, 2 a crit, 3 and 4 plain success, 5 and 6 the failure
//!   gauge. A record names the skills that acted on it: Lady Luck on the
//!   crit it caused, Plentiful Harvest and the Appraiser where they did.

/// A change to the player's gathering skills.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillChange {
    /// Every learned skill and its level (login).
    All(Vec<(u32, u32)>),
    /// Every skill back to 0.
    Reset,
    /// One skill now at `level`.
    Level { skill: u32, level: u32 },
}

/// How a gather goes, as its start says.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StartOutcome {
    pub node: u32,
    pub flags: u8,
    /// A crit on the success gauge (kind 2).
    pub crit: bool,
    /// A crit that filled the gauge at once (kind 1).
    pub instant: bool,
    /// Lady Luck caused a crit.
    pub lady_luck: bool,
}

impl StartOutcome {
    pub fn upgraded_at_start(&self) -> bool {
        self.flags & 0x04 != 0
    }

    /// The Appraiser upgraded it.
    pub fn appraised(&self) -> bool {
        self.flags & 0x02 != 0 && !self.upgraded_at_start()
    }
}

const LADY_LUCK: u32 = 300;
/// Skill ids are small (100 to 71200 seen); levels at most 10.
const MAX_SKILL: u32 = 100_000;
const MAX_LEVEL: u32 = 50;

fn body<'a>(packet: &'a [u8], op: [u8; 2]) -> Option<&'a [u8]> {
    let len = super::stream_processor::read_varint(packet, 0);
    let at = usize::try_from(len.length).ok().filter(|&n| n > 0)?;
    (packet.get(at..at + 2)? == op).then(|| &packet[at + 2..])
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

/// A change to the gathering skills, or None.
pub fn skills(packet: &[u8]) -> Option<SkillChange> {
    if let Some(b) = body(packet, [0x37, 0x8D]) {
        let n = *b.first()? as usize;
        let list = (0..n)
            .map(|i| Some((u32_at(b, 1 + 8 * i)?, u32_at(b, 5 + 8 * i)?)))
            .collect::<Option<Vec<_>>>()?;
        let sane = list.iter().all(|&(s, l)| (1..MAX_SKILL).contains(&s) && l <= MAX_LEVEL);
        return (sane && b.len() >= 1 + 8 * n).then_some(SkillChange::All(list));
    }
    if let Some(b) = body(packet, [0x3D, 0x8D]) {
        return (b.get(..2)? == [0, 0] && b.len() == 6).then_some(SkillChange::Reset);
    }
    let b = body(packet, [0x39, 0x8D]).or_else(|| body(packet, [0x3B, 0x8D]))?;
    if b.get(..2)? != [0, 0] || b.len() != 14 {
        return None;
    }
    let (skill, level) = (u32_at(b, 2)?, u32_at(b, 6)?);
    ((1..MAX_SKILL).contains(&skill) && level <= MAX_LEVEL).then_some(SkillChange::Level { skill, level })
}

/// One gauge record from `at`, `skip` being the stray byte or not: its
/// kind, the skills that acted on it, and where the next starts.
fn record(b: &[u8], at: usize, skip: usize) -> Option<(u8, Vec<u32>, usize)> {
    let kind = *b.get(at)?;
    let m = *b.get(at + 1 + skip)?;
    if m > 4 {
        return None;
    }
    let mut p = at + 2 + skip;
    let mut skills = Vec::with_capacity(m as usize);
    for _ in 0..m {
        let skill = u32_at(b, p).filter(|s| (1..MAX_SKILL).contains(s))?;
        (b.get(p + 4) == Some(&1)).then_some(())?;
        skills.push(skill);
        p += 9;
    }
    (b.len() >= p + 16).then_some((kind, skills, p + 16))
}

/// The `n` records from `at`, each read with or without its stray byte so
/// that all of them read and nothing but zeros is left after. A stray byte
/// taken for a record of no skills reads on, but not to the end.
fn records(b: &[u8], at: usize, n: u8) -> Option<Vec<(u8, Vec<u32>)>> {
    if n == 0 {
        return b.get(at..)?.iter().all(|&x| x == 0).then(Vec::new);
    }
    [0, 1].into_iter().find_map(|skip| {
        let (kind, skills, next) = record(b, at, skip)?;
        let mut rest = records(b, next, n - 1)?;
        rest.insert(0, (kind, skills));
        Some(rest)
    })
}

/// What a gather start (`31 8D 00 00`) says of the gather, or None.
pub fn start(packet: &[u8]) -> Option<StartOutcome> {
    use super::stream_processor::read_varint;
    let b = body(packet, [0x31, 0x8D])?;
    if b.get(..2)? != [0, 0] {
        return None;
    }
    let node = read_varint(b, 2);
    let at = usize::try_from(node.length).ok().filter(|&n| n > 0)? + 2 + 4;
    let flags = *b.get(at)?;
    let count = *b.get(at + 7)?;
    let mut out = StartOutcome { node: node.value as u32, flags, ..Default::default() };
    for (kind, skills) in records(b, at + 8, count)? {
        out.crit |= kind == 2;
        out.instant |= kind == 1;
        out.lady_luck |= skills.contains(&LADY_LUCK);
    }
    Some(out)
}

/// A gather done, with the skills the player had for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Gathered {
    /// Unix seconds.
    pub at: i64,
    pub item: u32,
    pub status: u8,
    pub count: u32,
    pub bonus: u32,
    pub extra_item: u32,
    pub start: StartOutcome,
    /// Every learned skill and its level; a skill not here is 0.
    pub skills: Vec<(u32, u32)>,
}

/// A start waits this long for its result.
const RESULT_WAIT_MS: i64 = 60_000;
/// Gathers kept unsent at most (the oldest go first).
const KEEP: usize = 1_000;

/// The player's skills as they change, and each gather joined to its start.
/// Until the skills are known (the login list, or a reset), gathers aren't
/// kept: a skill's level must be known for its procs to count.
#[derive(Debug, Default)]
pub struct Tracker {
    skills: Option<std::collections::BTreeMap<u32, u32>>,
    starts: std::collections::HashMap<u32, (i64, StartOutcome)>,
    done: Vec<Gathered>,
}

impl Tracker {
    pub fn skill_change(&mut self, change: SkillChange) {
        match change {
            SkillChange::All(list) => self.skills = Some(list.into_iter().collect()),
            SkillChange::Reset => self.skills = Some(Default::default()),
            SkillChange::Level { skill, level } => {
                if let Some(skills) = &mut self.skills {
                    skills.insert(skill, level);
                }
            }
        }
    }

    /// The skills as known: None before the login list or a reset.
    pub fn skills(&self) -> Option<Vec<(u32, u32)>> {
        self.skills.as_ref().map(|s| s.iter().map(|(&k, &v)| (k, v)).filter(|&(_, v)| v > 0).collect())
    }

    pub fn started(&mut self, at_ms: i64, start: StartOutcome) {
        self.starts.retain(|_, (t, _)| at_ms - *t < RESULT_WAIT_MS);
        self.starts.insert(start.node, (at_ms, start));
    }

    /// A result on `node`: kept with its start and the skills, when both are known.
    #[allow(clippy::too_many_arguments)]
    pub fn done(&mut self, at_ms: i64, node: u32, status: u8, item: u32, count: u32, bonus: u32, extra_item: u32) {
        let Some((t, start)) = self.starts.remove(&node) else { return };
        let Some(skills) = self.skills() else { return };
        if at_ms - t >= RESULT_WAIT_MS {
            return;
        }
        self.done.push(Gathered { at: at_ms / 1000, item, status, count, bonus, extra_item, start, skills });
        if self.done.len() > KEEP {
            self.done.remove(0);
        }
    }

    /// How many wait, and since when (unix s).
    pub fn waiting(&self) -> Option<(usize, i64)> {
        self.done.first().map(|g| (self.done.len(), g.at))
    }

    pub fn take(&mut self) -> Vec<Gathered> {
        std::mem::take(&mut self.done)
    }

    /// Gathers that couldn't be sent, back in front of any read since.
    pub fn put_back(&mut self, mut gathers: Vec<Gathered>) {
        gathers.append(&mut self.done);
        let over = gathers.len().saturating_sub(KEEP);
        gathers.drain(..over);
        self.done = gathers;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gathers_are_kept_only_with_known_skills() {
        let mut t = Tracker::default();
        let start = StartOutcome { node: 7, flags: 0x03, ..Default::default() };
        t.started(1_000, start);
        t.done(5_000, 7, 1, 610_670_045, 1, 0, 0);
        assert_eq!(t.waiting(), None, "skills not known yet");
        t.skill_change(SkillChange::All(vec![(800, 10), (51_200, 3)]));
        t.started(10_000, start);
        t.done(15_000, 8, 0, 610_670_041, 1, 0, 0);
        assert_eq!(t.waiting(), None, "another node's result");
        t.done(16_000, 7, 1, 610_670_045, 1, 0, 0);
        t.skill_change(SkillChange::Reset);
        t.skill_change(SkillChange::Level { skill: 800, level: 1 });
        t.started(20_000, start);
        t.done(90_000, 7, 1, 610_670_045, 1, 0, 0);
        assert_eq!(t.waiting(), Some((1, 16)), "a result a minute late is dropped");
        let sent = t.take();
        assert_eq!(sent[0].skills, vec![(800, 10), (51_200, 3)]);
        assert!(sent[0].start.appraised());
        assert_eq!(t.skills(), Some(vec![(800, 1)]), "after the reset");
        t.put_back(sent);
        assert_eq!(t.waiting(), Some((1, 16)));
    }

    fn hex(s: &str) -> Vec<u8> {
        let s: String = s.split_whitespace().collect();
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }

    /// The player's login list and their reset and reassigning (2026-11-17).
    #[test]
    fn skills_read_as_the_game_sends_them() {
        let login = hex("51 378d 09 64000000 0a000000 2c010000 05000000 20030000 0a000000 70c60000 01000000
            38c70000 03000000 00c80000 03000000 90140100 01000000 58150100 03000000 20160100 03000000 00000000");
        assert_eq!(
            skills(&login),
            Some(SkillChange::All(vec![(100, 10), (300, 5), (800, 10), (50_800, 1), (51_000, 3), (51_200, 3),
                (70_800, 1), (71_000, 3), (71_200, 3)]))
        );
        assert_eq!(skills(&hex("0a3d8d00005f000e00")), Some(SkillChange::Reset));
        assert_eq!(skills(&hex("12398d000020030000010000005e000e00")), Some(SkillChange::Level { skill: 800, level: 1 }));
        assert_eq!(skills(&hex("123b8d000020030000020000005c000e00")), Some(SkillChange::Level { skill: 800, level: 2 }));
        assert_eq!(skills(&hex("12398d063a20030000000000005e000e00")), None, "the echo");
    }

    #[test]
    fn a_gather_start_says_how_it_goes() {
        // Upgraded at start, with Lady Luck's crit.
        let luck = start(&hex("b801318d0000e1b502010000000700000200000006050164000000016e0000000000000000000000b400000064000000050164000000016e0000000000000000000000b400000064000000050164000000016e0000000000000000000000b40000006400000002022c010000013101000020030000012a030000fa0000002c0100000000000000000000040120030000012a0300003200000064000000000000000000000003000032000000320000000000000000000000")).unwrap();
        assert_eq!((luck.node, luck.flags, luck.crit, luck.instant, luck.lady_luck), (39_649, 0x07, true, false, true));
        assert!(luck.upgraded_at_start() && !luck.appraised());
        // A stray 00 before a record's skills, and Plentiful Harvest.
        let stray = start(&hex("dc01318d000082c101020000000100000100000007050164000000016e00000000000000000000009000000040000000030120030000012a0300006e000000a00000000000000000000000060164000000016e00000000000000000000006000000010000000040120030000012a03000032000000640000000000000000000000060164000000016e0000000000000000000000600000001000000005000164000000016e00000000000000000000009000000040000000040220030000012a03000038c70000013ac7000032000000640000000000000000000000")).unwrap();
        assert_eq!((stray.flags, stray.crit, stray.lady_luck), (0x01, false, false));
        assert!(!stray.appraised());
        // The Appraiser on step 5 (flags 0x83), and on step 8 with a stray 04 (flags 0x03).
        let fifth = start(&hex("9d01318d0000889701010000008300000200000005030120030000012a0300006e000000a00000000000000000000000050164000000016e00000000000000000000009000000040000000060164000000016e00000000000000000000006000000010000000040120030000012a03000032000000640000000000000000000000030100c800000102c800006400000064000000000000000000000000")).unwrap();
        assert!(fifth.appraised());
        let eighth = start(&hex("f701318d00008cd101030000000300000200000008040120030000012a03000032000000640000000000000000000000060164000000016e00000000000000000000006000000010000000060164000000016e00000000000000000000006000000010000000050164000000016e00000000000000000000009000000040000000040120030000012a0300003200000064000000000000000000000004040120030000012a03000032000000640000000000000000000000060164000000016e00000000000000000000006000000010000000010200c800000102c8000038c70000013ac700003c0000003c0000000000000000000000")).unwrap();
        assert!(eighth.appraised() && eighth.instant);
        assert_eq!(start(&hex("15 318d043a978402000000000000000000000000")), None, "refused");
    }
}
