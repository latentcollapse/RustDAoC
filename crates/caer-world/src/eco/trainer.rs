//! TrainerWindow 0x7B is authority. Local train intent never awards spec levels.

use caer_protocol::trainer::TrainerWindow;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrainIntent {
    OpenWindow,
    Train {
        id_line: u8,
        row: u8,
        skill_index: u8,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TrainerCorr {
    window: Option<TrainerWindow>,
    generation: u64,
    pending: Option<(TrainIntent, u64)>,
}

impl TrainerCorr {
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation
    }

    #[must_use]
    pub fn window(&self) -> Option<&TrainerWindow> {
        self.window.as_ref()
    }

    pub fn intent(&mut self, intent: TrainIntent) {
        self.pending = Some((intent, self.generation));
    }

    pub fn apply_window(&mut self, window: TrainerWindow, packet_generation: u64) -> bool {
        if packet_generation < self.generation {
            return false;
        }
        self.window = Some(window);
        self.generation = packet_generation;
        self.pending = None;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use caer_protocol::trainer::{encode_spec_window, TrainerLine};

    #[test]
    fn local_train_does_not_change_spec_levels() {
        let mut t = TrainerCorr::default();
        let body = encode_spec_window(
            12,
            &[TrainerLine {
                index: 0,
                level: 5,
                cost_or_next: 6,
                name: "Slash".into(),
            }],
        );
        let w = caer_protocol::trainer::decode(&body).unwrap();
        assert!(t.apply_window(w, 1));
        t.intent(TrainIntent::Train {
            id_line: 0,
            row: 1,
            skill_index: 1,
        });
        assert_eq!(t.window().unwrap().lines[0].level, 5);
        assert_eq!(t.window().unwrap().points, 12);
    }
}
