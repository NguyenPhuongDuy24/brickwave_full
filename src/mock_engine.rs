//! Local preview transport used only by Phase A.
//!
//! It neither opens an audio device nor touches the network. It emits the same
//! `PlayerEvent` stream a future backend worker will use, letting the UI be
//! tested without a SoundCloud account.

use crate::player::{PlayerCommand, PlayerController, PlayerEvent};

#[derive(Debug, Default)]
pub struct MockPlaybackEngine;

impl MockPlaybackEngine {
    pub fn advance(
        &mut self,
        controller: &mut PlayerController,
        elapsed_seconds: f32,
    ) -> Vec<PlayerEvent> {
        let state = controller.state();
        if !state.playback_status.is_playing() {
            return Vec::new();
        }

        let elapsed = elapsed_seconds.clamp(0.0, 0.25);
        let next_position = state.position_seconds + elapsed;
        let duration = state.duration_seconds;

        let mut events = controller.dispatch(PlayerCommand::Seek {
            seconds: next_position,
        });
        if duration.is_some_and(|seconds| next_position >= seconds) {
            events.extend(controller.dispatch(PlayerCommand::PlaybackFinished));
        }
        events
    }
}
