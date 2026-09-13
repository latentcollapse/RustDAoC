//! Encumberance 0xBD is authority. Local destroy/move never invents weight.

use caer_protocol::encumberance::Encumberance;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct EncumberanceCorr {
    value: Option<Encumberance>,
    generation: u64,
}

impl EncumberanceCorr {
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation
    }

    #[must_use]
    pub fn current(&self) -> Option<Encumberance> {
        self.value
    }

    pub fn apply_update(&mut self, update: Encumberance, packet_generation: u64) -> bool {
        if packet_generation < self.generation {
            return false;
        }
        self.value = Some(update);
        self.generation = packet_generation;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_encumberance_does_not_apply() {
        let mut c = EncumberanceCorr::default();
        assert!(c.apply_update(Encumberance { max: 100, used: 10 }, 2));
        assert!(!c.apply_update(Encumberance { max: 1, used: 1 }, 1));
        assert_eq!(c.current().unwrap().used, 10);
    }
}
