//! Load static world content (mob spawns) into the world model from a data dump.
//!
//! This is the "cheat" made real: the entire realm's population is pre-catalogued in the
//! server's `Mob` table, so a client can *load* it up front rather than wait to see each mob
//! stream in over the wire. The packet stream then just updates/overrides the live subset. It's
//! also how we feed the benchmark a real realm-scale population instead of a synthetic one.
//!
//! Input format is one mob per line, tab-separated, as dumped from the DB:
//! `Mob_ID  Name  X  Y  Z  Heading  Model  Level  Realm  Region`
//! (Mob_ID and Region are ignored here; ids are assigned sequentially on load.)

use std::io::BufRead;

use caer_protocol::entities::Npc;

/// Parse a mob dump into `Npc` records, assigning sequential object ids from `1`. Malformed or
/// short lines are skipped rather than failing the whole load (a dump is best-effort static
/// data, not a protocol stream). Ids beyond `u16::MAX` are not emitted — a single region stays
/// well under that, which is the intended unit of loading.
pub fn load_mobs_tsv(reader: impl BufRead) -> Vec<Npc> {
    let mut out = Vec::new();
    let mut next_id: u32 = 1;
    for line in reader.lines().map_while(Result::ok) {
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() < 9 {
            continue;
        }
        let (Ok(x), Ok(y), Ok(z), Ok(heading), Ok(model), Ok(level), Ok(realm)) = (
            f[2].parse::<i64>(),
            f[3].parse::<i64>(),
            f[4].parse::<i64>(),
            f[5].parse::<u16>(),
            f[6].parse::<u16>(),
            f[7].parse::<u8>(),
            f[8].parse::<u8>(),
        ) else {
            continue;
        };
        if next_id > u32::from(u16::MAX) {
            break;
        }
        out.push(Npc {
            object_id: next_id as u16,
            speed: 0, // spawns are stationary until the server moves them
            heading: heading % 4096,
            x: x.max(0) as u32,
            y: y.max(0) as u32,
            z: z.clamp(0, i64::from(u16::MAX)) as u16,
            model,
            size: 50,
            level,
            flags: realm << 6,
            name: f[1].to_string(),
            guild: String::new(),
        });
        next_id += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::WorldState;
    use caer_protocol::session::ServerEvent;

    const SAMPLE: &str = "\
guid-1\tpike\t592241\t538249\t1676\t1669\t33735\t0\t0\t1
guid-2\tgranite giant outlooker\t373922\t744816\t1707\t1149\t615\t0\t0\t1
bad-line-too-short\tx\t1
guid-3\triver spriteling\t488120\t609958\t1624\t1935\t136\t0\t0\t1";

    #[test]
    fn parses_and_skips_bad_lines() {
        let mobs = load_mobs_tsv(SAMPLE.as_bytes());
        assert_eq!(mobs.len(), 3, "3 good lines, 1 short line skipped");
        assert_eq!(mobs[0].name, "pike");
        assert_eq!((mobs[0].x, mobs[0].y, mobs[0].z), (592241, 538249, 1676));
        assert_eq!(mobs[1].name, "granite giant outlooker");
        assert_eq!(mobs[0].object_id, 1);
        assert_eq!(
            mobs[2].object_id, 3,
            "ids stay sequential across a skipped line"
        );
    }

    #[test]
    fn loads_into_world_model() {
        let mut w = WorldState::new();
        for npc in load_mobs_tsv(SAMPLE.as_bytes()) {
            w.apply(&ServerEvent::NpcInView(npc));
        }
        assert_eq!(w.len(), 3);
        assert_eq!(w.get(1).unwrap().name, "pike");
        // spatial query works on the loaded population
        assert_eq!(w.count_within_indexed([592241, 538249], 1000), 1);
    }
}
