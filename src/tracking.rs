use crate::vision::{Detection, DetectionKind};
use std::time::{Duration, Instant};

const TARGET_LOST_TIMEOUT: Duration = Duration::from_millis(750);
const REACQUIRED_DISPLAY_TIME: Duration = Duration::from_millis(750);

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum TrackingState {
    Locked,
    Following,
    Searching,
    Lost,
    Reacquired,
}

impl TrackingState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Locked => "LOCKED",
            Self::Following => "FOLLOWING",
            Self::Searching => "SEARCHING",
            Self::Lost => "LOST",
            Self::Reacquired => "REACQUIRED",
        }
    }
}

#[derive(Clone)]
pub struct SelectedTarget {
    pub detection: Detection,
    pub visible: bool,
    state: TrackingState,
    missing_since: Option<Instant>,
    reacquired_at: Option<Instant>,
}

impl SelectedTarget {
    pub fn new(detection: Detection) -> Self {
        Self {
            detection,
            visible: true,
            state: TrackingState::Locked,
            missing_since: None,
            reacquired_at: None,
        }
    }

    pub fn state(&self) -> TrackingState {
        self.state
    }

    pub fn refresh(&mut self, detections: &[Detection], tracking_enabled: bool) {
        self.refresh_at(detections, tracking_enabled, Instant::now());
    }

    fn refresh_at(&mut self, detections: &[Detection], tracking_enabled: bool, now: Instant) {
        if let Some(detection) = self.matching_detection(detections) {
            let was_visible = self.visible;

            self.detection = detection;
            self.visible = true;
            self.missing_since = None;

            if !was_visible {
                self.state = TrackingState::Reacquired;
                self.reacquired_at = Some(now);
                return;
            }

            if self.state == TrackingState::Reacquired {
                if self.reacquired_at.is_some_and(|reacquired_at| {
                    now.saturating_duration_since(reacquired_at) < REACQUIRED_DISPLAY_TIME
                }) {
                    return;
                }

                self.reacquired_at = None;
            }

            self.state = if tracking_enabled {
                TrackingState::Following
            } else {
                TrackingState::Locked
            };

            return;
        }

        if self.visible {
            self.missing_since = Some(now);
        }

        self.visible = false;
        self.reacquired_at = None;

        let missing_since = *self.missing_since.get_or_insert(now);

        self.state = if now.saturating_duration_since(missing_since) >= TARGET_LOST_TIMEOUT {
            TrackingState::Lost
        } else {
            TrackingState::Searching
        };
    }

    fn matching_detection(&self, detections: &[Detection]) -> Option<Detection> {
        if let Some(detection) = detections.iter().find(|detection| {
            detection.kind == self.detection.kind && detection.id == self.detection.id
        }) {
            return Some(detection.clone());
        }

        let current_x = (self.detection.x1 + self.detection.x2) * 0.5;
        let current_y = (self.detection.y1 + self.detection.y2) * 0.5;

        let current_width = self.detection.x2 - self.detection.x1;
        let current_height = self.detection.y2 - self.detection.y1;

        let maximum_distance = match self.detection.kind {
            DetectionKind::Face => current_width
                .hypot(current_height)
                .mul_add(0.45, 0.0)
                .max(60.0),
            DetectionKind::Person => current_width
                .hypot(current_height)
                .mul_add(0.75, 0.0)
                .max(80.0),
        };

        let mut best: Option<(&Detection, f32)> = None;

        for detection in detections
            .iter()
            .filter(|detection| detection.kind == self.detection.kind)
        {
            let center_x = (detection.x1 + detection.x2) * 0.5;
            let center_y = (detection.y1 + detection.y2) * 0.5;

            let distance = (center_x - current_x).hypot(center_y - current_y);

            if best
                .as_ref()
                .is_none_or(|(_, best_distance)| distance < *best_distance)
            {
                best = Some((detection, distance));
            }
        }

        best.and_then(|(detection, distance)| {
            (distance <= maximum_distance).then(|| detection.clone())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn face(id: u64, x: f32, y: f32) -> Detection {
        Detection {
            kind: DetectionKind::Face,
            x1: x,
            y1: y,
            x2: x + 100.0,
            y2: y + 100.0,
            confidence: 0.9,
            id,
        }
    }

    #[test]
    fn following_target_moves_through_search_lost_and_reacquired_states() {
        let start = Instant::now();
        let detection = face(3, 100.0, 100.0);
        let mut target = SelectedTarget::new(detection.clone());

        assert_eq!(target.state(), TrackingState::Locked);

        target.refresh_at(std::slice::from_ref(&detection), true, start);
        assert_eq!(target.state(), TrackingState::Following);

        target.refresh_at(&[], true, start + Duration::from_millis(10));
        assert_eq!(target.state(), TrackingState::Searching);
        assert!(!target.visible);

        target.refresh_at(
            &[],
            true,
            start + TARGET_LOST_TIMEOUT + Duration::from_millis(20),
        );
        assert_eq!(target.state(), TrackingState::Lost);

        let reacquired_at = start + TARGET_LOST_TIMEOUT + Duration::from_millis(30);

        target.refresh_at(std::slice::from_ref(&detection), true, reacquired_at);
        assert_eq!(target.state(), TrackingState::Reacquired);
        assert!(target.visible);

        target.refresh_at(
            std::slice::from_ref(&detection),
            true,
            reacquired_at + REACQUIRED_DISPLAY_TIME + Duration::from_millis(1),
        );
        assert_eq!(target.state(), TrackingState::Following);
    }

    #[test]
    fn reacquired_target_returns_to_locked_when_follow_is_disabled() {
        let start = Instant::now();
        let detection = face(7, 200.0, 150.0);
        let mut target = SelectedTarget::new(detection.clone());

        target.refresh_at(&[], false, start);
        assert_eq!(target.state(), TrackingState::Searching);

        let reacquired_at = start + Duration::from_millis(100);

        target.refresh_at(std::slice::from_ref(&detection), false, reacquired_at);
        assert_eq!(target.state(), TrackingState::Reacquired);

        target.refresh_at(
            std::slice::from_ref(&detection),
            false,
            reacquired_at + REACQUIRED_DISPLAY_TIME + Duration::from_millis(1),
        );
        assert_eq!(target.state(), TrackingState::Locked);
    }
}
