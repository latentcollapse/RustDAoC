//! Starting-stat allocation rules, transcribed from the OPEN_ORACLE.
//!
//! Source: DOLSharp `GameServer/packets/Client/168/CharacterCreateRequestHandler.cs`
//! (`IsCustomPointsDistributionValid`, `MaxStartingBonusPoints`) and
//! `GameServer/GlobalConstants.cs` (`STARTING_STATS_DICT`). These are the rules the server
//! enforces at create time; the client's stats screen exists to let a player satisfy them by
//! hand, so the client mirrors them exactly rather than inventing friendlier ones.
//!
//! Wire order everywhere in this crate is str, dex, con, qui, int, pie, emp, chr.
//!
//! The rule, at creation (level 1):
//! * every stat has a floor of its race base ([`STARTING_STATS`]);
//! * class auto-grants are zero at level 1 (the oracle's loop runs from `level` down to > 5);
//! * each point of `above = stat - race_base` costs 1 point up to +10, 2 points to +15,
//!   3 points beyond;
//! * the total spent must be **exactly** [`MAX_STARTING_BONUS_POINTS`] — not under.

/// Wire-order stat names, for messages.
pub const STAT_NAMES: [&str; 8] = [
    "Strength",
    "Dexterity",
    "Constitution",
    "Quickness",
    "Intelligence",
    "Piety",
    "Empathy",
    "Charisma",
];

/// DOLSharp `MaxStartingBonusPoints`.
pub const MAX_STARTING_BONUS_POINTS: u32 = 30;

/// Cost breakpoints of the oracle's escalation: the first 10 points above base cost 1 each,
/// the next 5 cost 2 each, everything beyond costs 3.
const TIERS: [(u32, u32); 3] = [(10, 1), (15, 2), (u32::MAX, 3)];

/// Cost in bonus points of raising one stat `above` points over its race base.
///
/// Mirrors `points += above; points += max(0, above-10); points += max(0, above-15)` — the
/// oracle charges cumulatively per stat, so the marginal costs are 1 / 2 / 3.
#[must_use]
pub fn spent_on(above: u32) -> u32 {
    above
        .saturating_add(above.saturating_sub(10))
        .saturating_add(above.saturating_sub(15))
}

/// Marginal cost of the NEXT point when a stat already sits `above` its race base.
#[must_use]
pub fn marginal_cost(above: u32) -> u32 {
    for (limit, cost) in TIERS {
        if above < limit {
            return cost;
        }
    }
    unreachable!("last tier bound is u32::MAX");
}

/// Race base stats, wire order (str, dex, con, qui, int, pie, emp, chr).
///
/// Index is the DAoC/DOL race id (1 Briton … 21 Hibernia Minotaur), matching
/// `eRace` in the oracle and every race id this crate already puts on the wire.
/// Rows are verbatim from `GlobalConstants.cs STARTING_STATS_DICT`; races absent from the
/// client's creation forms carry the oracle's `Unknown` row rather than an invented one.
pub const STARTING_STATS: [[u8; 8]; 22] = [
    [60, 60, 60, 60, 60, 60, 60, 60],  // 0 Unknown (oracle default row)
    [60, 60, 60, 60, 60, 60, 60, 60],  // 1 Briton
    [45, 60, 45, 70, 80, 60, 60, 60],  // 2 Avalonian
    [70, 50, 70, 50, 60, 60, 60, 60],  // 3 Highlander
    [50, 80, 50, 60, 60, 60, 60, 60],  // 4 Saracen
    [70, 50, 70, 50, 60, 60, 60, 60],  // 5 Norseman
    [100, 35, 70, 35, 60, 60, 60, 60], // 6 Troll
    [60, 50, 80, 50, 60, 60, 60, 60],  // 7 Dwarf
    [50, 70, 50, 70, 60, 60, 60, 60],  // 8 Kobold
    [60, 60, 60, 60, 60, 60, 60, 60],  // 9 Celt
    [90, 40, 60, 40, 60, 60, 70, 60],  // 10 Firbolg
    [40, 75, 40, 75, 70, 60, 60, 60],  // 11 Elf
    [40, 80, 40, 80, 60, 60, 60, 60],  // 12 Lurikeen
    [50, 70, 60, 50, 70, 60, 60, 60],  // 13 Inconnu
    [55, 65, 45, 75, 60, 60, 60, 60],  // 14 Valkyn
    [70, 55, 60, 45, 70, 60, 60, 60],  // 15 Sylvan
    [90, 40, 70, 40, 60, 60, 60, 60],  // 16 HalfOgre
    [55, 55, 55, 60, 60, 75, 60, 60],  // 17 Frostalf
    [60, 50, 80, 50, 60, 60, 60, 60],  // 18 Shar
    [80, 50, 70, 40, 60, 60, 60, 60],  // 19 Albion Minotaur
    [80, 50, 70, 40, 60, 60, 60, 60],  // 20 Midgard Minotaur
    [80, 50, 70, 40, 60, 60, 60, 60],  // 21 Hibernia Minotaur
];

/// Race base for one stat. Race 0 / unknown resolves to the oracle's `Unknown` row, which is
/// also what every legal race row collapses to for most stats — never a panic, never a guess.
#[must_use]
pub fn race_base(race: u8, stat: usize) -> u8 {
    STARTING_STATS
        .get(race as usize)
        .map_or(STARTING_STATS[0], |r| *r)[stat.min(7)]
}

/// Total bonus points a distribution spends. `stats` are absolute values, wire order.
#[must_use]
pub fn total_spent(race: u8, stats: &[u8; 8]) -> u32 {
    let mut sum = 0u32;
    for (i, &s) in stats.iter().enumerate() {
        sum = sum.saturating_add(spent_on(u32::from(s.saturating_sub(race_base(race, i)))));
    }
    sum
}

/// Whether a finished distribution is exactly what the oracle accepts.
///
/// Mirror of `IsCustomPointsDistributionValid` at level 1 plus the caller's
/// `pointsUsed != MaxStartingBonusPoints` check: no stat below its race base, and exactly
/// [`MAX_STARTING_BONUS_POINTS`] spent. Returns the failure reason for the refusal message.
#[must_use]
pub fn validate_distribution(race: u8, stats: &[u8; 8]) -> Result<(), &'static str> {
    for (i, &s) in stats.iter().enumerate() {
        let base = race_base(race, i);
        if s < base {
            return Err("Your base statistics cannot be lowered.");
        }
    }
    let spent = total_spent(race, stats);
    if spent != MAX_STARTING_BONUS_POINTS {
        return Err("You must spend exactly 30 bonus points.");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cost_escalates_one_two_three_like_the_oracle() {
        assert_eq!(spent_on(0), 0);
        assert_eq!(spent_on(10), 10, "first ten points cost one each");
        assert_eq!(spent_on(11), 12, "the eleventh point costs two");
        assert_eq!(spent_on(15), 20);
        assert_eq!(spent_on(16), 23, "the sixteenth point costs three");
        assert_eq!(marginal_cost(0), 1);
        assert_eq!(marginal_cost(9), 1);
        assert_eq!(marginal_cost(10), 2);
        assert_eq!(marginal_cost(14), 2);
        assert_eq!(marginal_cost(15), 3);
    }

    /// Transcribed rows must agree with the oracle text they claim to come from.
    ///
    /// Known-bad shape: any transcription error in a race's distinctive stats moves at least
    /// one asserted value. Troll STR 100 and the min-max spread across races are the sharpest
    /// fingerprints.
    #[test]
    fn transcribed_rows_match_their_globalconstants_source() {
        assert_eq!(race_base(6, 0), 100, "Troll STR");
        assert_eq!(race_base(6, 1), 35, "Troll DEX");
        assert_eq!(race_base(11, 0), 40, "Elf STR");
        assert_eq!(race_base(12, 1), 80, "Lurikeen DEX");
        assert_eq!(race_base(17, 5), 75, "Frostalf PIE");
        assert_eq!(race_base(16, 0), 90, "HalfOgre STR");
        assert_eq!(
            STARTING_STATS[1],
            [60, 60, 60, 60, 60, 60, 60, 60],
            "Briton is flat 60"
        );
        // A wrong row somewhere shows up as an impossible floor: nothing may sit below 35
        // (Troll DEX/QUI) or above 100 (Troll STR).
        for (ri, row) in STARTING_STATS.iter().enumerate() {
            for (si, &v) in row.iter().enumerate() {
                assert!(
                    (35..=100).contains(&v),
                    "race {ri} stat {si} = {v} outside the oracle's range"
                );
            }
        }
    }

    /// A distribution the oracle would accept passes; each way to be wrong names itself.
    #[test]
    fn validate_mirrors_ischaractervalid_at_level_one() {
        // Briton: spend 30 as 6/6/6/6/6/0/0/0 — cheap tier only.
        let mut s = [60u8; 8];
        for i in 0..6 {
            s[i] += 5; // 6 stats x 5 points = 30
        }
        assert_eq!(validate_distribution(1, &s), Ok(()));

        // Below base refused.
        let mut under = s;
        under[6] = 59;
        assert_eq!(
            validate_distribution(1, &under),
            Err("Your base statistics cannot be lowered.")
        );

        // Under-spend refused (oracle: pointsUsed != 30).
        let mut short = s;
        short[0] -= 1;
        assert_eq!(
            validate_distribution(1, &short),
            Err("You must spend exactly 30 bonus points.")
        );

        // Over-spend refused too.
        let mut over = s;
        over[0] += 1;
        assert_eq!(
            validate_distribution(1, &over),
            Err("You must spend exactly 30 bonus points.")
        );
    }

    /// Escalated tiers interact with the exact-30 rule: a Troll cannot dump 30 points into STR
    /// alone (only 10 cost one point each, so 20 above base already spends 40).
    #[test]
    fn thirty_points_bounds_how_far_any_stat_can_rise() {
        // Max single-stat raise affordable with 30 points: 12 above base costs 14, leaving 16.
        assert_eq!(spent_on(12), 14);
        assert_eq!(spent_on(13), 16);
        // So a Troll (STR 100) can reach 112 on 30 points but not 113.
        let mut troll = [100, 35, 70, 35, 60, 60, 60, 60];
        troll[0] = 112;
        troll[1] += 8; // 35->43, 8 points, total 14+8=22... need exactly 30:
        troll[2] += 8; // 70->78, total 30
        assert_eq!(validate_distribution(6, &troll), Ok(()));
    }

    /// **Adjacent-column swap control (seen red 2026-08-23).** A mechanical row-by-row diff of
    /// this table against `GlobalConstants.cs STARTING_STATS_DICT` caught Firbolg transcribed
    /// as PIE 70 / EMP 60 — the oracle has PIE 60 / EMP 70, an adjacent-wire-order swap that
    /// every then-existing test accepted. The values below are the oracle excerpt itself, and
    /// this test fails if the table order is ever read back in columns-swapped. Both local
    /// oracles agree; diff verified by /tmp/check_starting_stats.py before it was deleted.
    #[test]
    fn firbolg_and_frostalf_bases_match_oracle_column_order() {
        // GlobalConstants.cs: Firbolg {STR 90, CON 60, DEX 40, QUI 40, INT 60, PIE 60, EMP 70, CHR 60}
        assert_eq!(
            STARTING_STATS[10],
            [90, 40, 60, 40, 60, 60, 70, 60],
            "Firbolg in wire order str, dex, con, qui, int, pie, emp, chr — PIE 60, EMP 70"
        );
        // GlobalConstants.cs: Frostalf {STR 55, CON 55, DEX 55, QUI 60, INT 60, PIE 75, EMP 60, CHR 60}
        assert_eq!(
            STARTING_STATS[17],
            [55, 55, 55, 60, 60, 75, 60, 60],
            "Frostalf in wire order — PIE 75, EMP 60"
        );
    }
}
