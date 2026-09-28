//! Player domain model independent of egui, network and audio output.
//!
//! A future SoundCloud worker will receive `PlayerCommand` and produce the same
//! `PlayerEvent` values as the local preview engine. The UI only reads
//! `PlayerState` from `PlayerController`.

use std::collections::BTreeMap;

use crate::state::{PlaybackAvailability, TrackId};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Ord, PartialOrd)]
pub struct QueueEntryId(u64);

impl QueueEntryId {
    pub const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct QueueEntry {
    pub id: QueueEntryId,
    pub track_id: TrackId,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PlaybackStatus {
    Idle,
    Loading,
    Playing,
    Paused,
    Ended,
    Error(String),
}

impl PlaybackStatus {
    pub const fn is_playing(&self) -> bool {
        matches!(self, Self::Playing)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RepeatMode {
    Off,
    All,
    One,
}

impl RepeatMode {
    pub const fn next(self) -> Self {
        match self {
            Self::Off => Self::All,
            Self::All => Self::One,
            Self::One => Self::Off,
        }
    }
}

/// The single source of player truth consumed by every UI surface.
#[derive(Clone, Debug)]
pub struct PlayerState {
    pub current_track_id: Option<TrackId>,
    pub current_queue_entry_id: Option<QueueEntryId>,
    pub queue: Vec<QueueEntry>,
    pub playback_status: PlaybackStatus,
    pub position_seconds: f32,
    pub duration_seconds: Option<f32>,
    pub volume: f32,
    pub shuffle_enabled: bool,
    pub repeat_mode: RepeatMode,
    play_order: Vec<QueueEntryId>,
}

impl Default for PlayerState {
    fn default() -> Self {
        Self {
            current_track_id: None,
            current_queue_entry_id: None,
            queue: Vec::new(),
            playback_status: PlaybackStatus::Idle,
            position_seconds: 0.0,
            duration_seconds: None,
            volume: 0.72,
            shuffle_enabled: false,
            repeat_mode: RepeatMode::Off,
            play_order: Vec::new(),
        }
    }
}

impl PlayerState {
    #[allow(dead_code)] // Public backend-facing contract; used by unit tests in Phase A.
    pub fn play_order(&self) -> &[QueueEntryId] {
        &self.play_order
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum PlayerCommand {
    PlayTrack {
        track_id: TrackId,
        context: Vec<TrackId>,
    },
    /// Selects metadata and builds the queue without claiming audio playback.
    /// The LIVE metadata UI uses this until an audio backend exists.
    SelectTrack {
        track_id: TrackId,
        context: Vec<TrackId>,
    },
    SelectCollection {
        tracks: Vec<TrackId>,
        start_at: usize,
    },
    PlayCollection {
        tracks: Vec<TrackId>,
        start_at: usize,
    },
    /// Stops transport while preserving the selected track and queue.
    /// A later Play command restarts the selected track from the beginning.
    Stop,
    TogglePlayPause,
    Next,
    Previous,
    /// Moves metadata selection without claiming that audio started.
    SelectNext,
    SelectPrevious,
    SelectQueueEntry {
        entry_id: QueueEntryId,
    },
    PlayQueueEntry {
        entry_id: QueueEntryId,
    },
    Seek {
        seconds: f32,
    },
    SetShuffle(bool),
    CycleRepeat,
    SetRepeat(RepeatMode),
    SetVolume(f32),
    AddQueueEntry {
        track_id: TrackId,
    },
    RemoveQueueEntry {
        entry_id: QueueEntryId,
    },
    MoveQueueEntry {
        entry_id: QueueEntryId,
        to_index: usize,
    },
    PlaybackFinished,
}

#[derive(Clone, Debug, PartialEq)]
pub enum PlayerEvent {
    TrackLoading {
        track_id: TrackId,
        entry_id: QueueEntryId,
    },
    TrackPlaying {
        track_id: TrackId,
        entry_id: QueueEntryId,
    },
    TrackPaused {
        track_id: TrackId,
        entry_id: QueueEntryId,
    },
    TrackStopped {
        track_id: TrackId,
        entry_id: QueueEntryId,
    },
    PositionChanged {
        seconds: f32,
    },
    TrackEnded {
        track_id: TrackId,
        entry_id: QueueEntryId,
    },
    QueueChanged,
    PlaybackError {
        message: String,
    },
}

/// Command processor. It owns generated queue-entry identities and a small
/// deterministic PRNG only for rebuilding a shuffle order after a command.
#[derive(Debug)]
pub struct PlayerController {
    state: PlayerState,
    track_metadata: BTreeMap<TrackId, TrackPlaybackMetadata>,
    next_queue_entry_id: u64,
    shuffle_seed: u64,
}

/// The player needs only availability and duration. Full display metadata
/// remains in `UiState::catalog`, avoiding a second track collection in UI.
#[derive(Clone, Copy, Debug)]
pub struct TrackPlaybackMetadata {
    pub duration_seconds: Option<u32>,
    pub availability: PlaybackAvailability,
}

impl Default for PlayerController {
    fn default() -> Self {
        Self::new()
    }
}

impl PlayerController {
    pub fn new() -> Self {
        Self {
            state: PlayerState::default(),
            track_metadata: BTreeMap::new(),
            next_queue_entry_id: 1,
            shuffle_seed: 0x51_2f_9a_77_04_29_11,
        }
    }

    pub fn state(&self) -> &PlayerState {
        &self.state
    }

    /// Marks the selected queue entry as waiting for the real audio backend.
    /// Unlike the preview `Play*` commands, this does not claim sound started.
    pub fn begin_loading(&mut self) -> Vec<PlayerEvent> {
        let (Some(track_id), Some(entry_id)) = (
            self.state.current_track_id,
            self.state.current_queue_entry_id,
        ) else {
            return self.error("Choose a track before starting playback");
        };
        self.state.playback_status = PlaybackStatus::Loading;
        self.state.position_seconds = 0.0;
        vec![PlayerEvent::TrackLoading { track_id, entry_id }]
    }

    pub fn confirm_playing(
        &mut self,
        track_id: TrackId,
        entry_id: QueueEntryId,
    ) -> Vec<PlayerEvent> {
        if !self.is_current(track_id, entry_id) {
            return Vec::new();
        }
        self.state.playback_status = PlaybackStatus::Playing;
        vec![PlayerEvent::TrackPlaying { track_id, entry_id }]
    }

    pub fn confirm_paused(
        &mut self,
        track_id: TrackId,
        entry_id: QueueEntryId,
    ) -> Vec<PlayerEvent> {
        if !self.is_current(track_id, entry_id) {
            return Vec::new();
        }
        self.state.playback_status = PlaybackStatus::Paused;
        vec![PlayerEvent::TrackPaused { track_id, entry_id }]
    }

    pub fn update_position(
        &mut self,
        track_id: TrackId,
        entry_id: QueueEntryId,
        seconds: f32,
    ) -> Vec<PlayerEvent> {
        if !self.is_current(track_id, entry_id) || !seconds.is_finite() {
            return Vec::new();
        }
        let seconds = if let Some(duration) = self.state.duration_seconds {
            seconds.clamp(0.0, duration)
        } else {
            seconds.max(0.0)
        };
        self.state.position_seconds = seconds;
        vec![PlayerEvent::PositionChanged { seconds }]
    }

    pub fn playback_error(&mut self, message: impl Into<String>) -> Vec<PlayerEvent> {
        let message = message.into();
        self.state.playback_status = PlaybackStatus::Error(message.clone());
        vec![PlayerEvent::PlaybackError { message }]
    }

    fn is_current(&self, track_id: TrackId, entry_id: QueueEntryId) -> bool {
        self.state.current_track_id == Some(track_id)
            && self.state.current_queue_entry_id == Some(entry_id)
    }

    pub fn sync_track_metadata<I>(&mut self, metadata: I)
    where
        I: IntoIterator<Item = (TrackId, TrackPlaybackMetadata)>,
    {
        self.track_metadata = metadata.into_iter().collect();
    }

    pub fn dispatch(&mut self, command: PlayerCommand) -> Vec<PlayerEvent> {
        match command {
            PlayerCommand::PlayTrack {
                track_id,
                mut context,
            } => {
                if context.is_empty() {
                    context.push(track_id);
                }
                let start_at = context.iter().position(|id| *id == track_id).unwrap_or(0);
                self.play_collection(context, start_at)
            }
            PlayerCommand::SelectTrack {
                track_id,
                mut context,
            } => {
                if context.is_empty() {
                    context.push(track_id);
                }
                let start_at = context.iter().position(|id| *id == track_id).unwrap_or(0);
                self.select_collection(context, start_at)
            }
            PlayerCommand::PlayCollection { tracks, start_at } => {
                self.play_collection(tracks, start_at)
            }
            PlayerCommand::SelectCollection { tracks, start_at } => {
                self.select_collection(tracks, start_at)
            }
            PlayerCommand::Stop => self.stop(),
            PlayerCommand::TogglePlayPause => self.toggle_play_pause(),
            PlayerCommand::Next => self.next(),
            PlayerCommand::Previous => self.previous(),
            PlayerCommand::SelectNext => self.next_with_status(PlaybackStatus::Paused),
            PlayerCommand::SelectPrevious => self.previous_with_status(PlaybackStatus::Paused),
            PlayerCommand::SelectQueueEntry { entry_id } => {
                self.activate_existing_entry(entry_id, PlaybackStatus::Paused)
            }
            PlayerCommand::PlayQueueEntry { entry_id } => {
                self.activate_existing_entry(entry_id, PlaybackStatus::Playing)
            }
            PlayerCommand::Seek { seconds } => self.seek(seconds),
            PlayerCommand::SetShuffle(enabled) => self.set_shuffle(enabled),
            PlayerCommand::CycleRepeat => {
                self.state.repeat_mode = self.state.repeat_mode.next();
                Vec::new()
            }
            PlayerCommand::SetRepeat(mode) => {
                self.state.repeat_mode = mode;
                Vec::new()
            }
            PlayerCommand::SetVolume(volume) => {
                self.state.volume = volume.clamp(0.0, 1.0);
                Vec::new()
            }
            PlayerCommand::AddQueueEntry { track_id } => self.add_queue_entry(track_id),
            PlayerCommand::RemoveQueueEntry { entry_id } => self.remove_queue_entry(entry_id),
            PlayerCommand::MoveQueueEntry { entry_id, to_index } => {
                self.move_queue_entry(entry_id, to_index)
            }
            PlayerCommand::PlaybackFinished => self.playback_finished(),
        }
    }

    fn play_collection(&mut self, tracks: Vec<TrackId>, start_at: usize) -> Vec<PlayerEvent> {
        self.replace_collection(tracks, start_at, PlaybackStatus::Playing)
    }

    fn select_collection(&mut self, tracks: Vec<TrackId>, start_at: usize) -> Vec<PlayerEvent> {
        self.replace_collection(tracks, start_at, PlaybackStatus::Paused)
    }

    fn replace_collection(
        &mut self,
        tracks: Vec<TrackId>,
        start_at: usize,
        status: PlaybackStatus,
    ) -> Vec<PlayerEvent> {
        if tracks.is_empty() {
            return self.error("Cannot play an empty collection");
        }
        if start_at >= tracks.len() {
            return self.error("Selected track is outside the play context");
        }
        if let Some(id) = tracks
            .iter()
            .copied()
            .find(|id| !self.track_metadata.contains_key(id))
        {
            return self.error(format!("Unknown track {}", id.get()));
        }

        self.state.queue = tracks
            .into_iter()
            .map(|track_id| QueueEntry {
                id: self.allocate_entry_id(),
                track_id,
            })
            .collect();
        let selected_id = self.state.queue[start_at].id;
        self.rebuild_play_order(Some(selected_id));

        let mut events = vec![PlayerEvent::QueueChanged];
        events.extend(self.activate(selected_id, status));
        events
    }

    fn toggle_play_pause(&mut self) -> Vec<PlayerEvent> {
        let Some(entry_id) = self.state.current_queue_entry_id else {
            return self.error("Choose a preview track before pressing play");
        };
        let Some(track_id) = self.state.current_track_id else {
            return self.error("Current queue entry has no track");
        };

        match self.state.playback_status {
            PlaybackStatus::Playing => {
                self.state.playback_status = PlaybackStatus::Paused;
                vec![PlayerEvent::TrackPaused { track_id, entry_id }]
            }
            PlaybackStatus::Paused | PlaybackStatus::Ended | PlaybackStatus::Loading => {
                self.state.playback_status = PlaybackStatus::Playing;
                vec![PlayerEvent::TrackPlaying { track_id, entry_id }]
            }
            PlaybackStatus::Idle | PlaybackStatus::Error(_) => {
                self.activate(entry_id, PlaybackStatus::Playing)
            }
        }
    }

    fn stop(&mut self) -> Vec<PlayerEvent> {
        let (Some(track_id), Some(entry_id)) = (
            self.state.current_track_id,
            self.state.current_queue_entry_id,
        ) else {
            self.state.playback_status = PlaybackStatus::Idle;
            self.state.position_seconds = 0.0;
            return Vec::new();
        };
        self.state.playback_status = PlaybackStatus::Idle;
        self.state.position_seconds = 0.0;
        vec![PlayerEvent::TrackStopped { track_id, entry_id }]
    }

    fn next(&mut self) -> Vec<PlayerEvent> {
        self.next_with_status(PlaybackStatus::Playing)
    }

    fn next_with_status(&mut self, status: PlaybackStatus) -> Vec<PlayerEvent> {
        let Some(current) = self.state.current_queue_entry_id else {
            return self.error("Queue is empty");
        };
        let Some(position) = self.play_order_index(current) else {
            return self.error("Current queue entry is missing");
        };
        if let Some(next) = self.state.play_order.get(position + 1).copied() {
            return self.activate(next, status.clone());
        }
        if self.state.repeat_mode == RepeatMode::All {
            if let Some(first) = self.state.play_order.first().copied() {
                return self.activate(first, status);
            }
        }
        Vec::new()
    }

    fn previous(&mut self) -> Vec<PlayerEvent> {
        self.previous_with_status(PlaybackStatus::Playing)
    }

    fn previous_with_status(&mut self, status: PlaybackStatus) -> Vec<PlayerEvent> {
        let Some(current) = self.state.current_queue_entry_id else {
            return self.error("Queue is empty");
        };
        let Some(position) = self.play_order_index(current) else {
            return self.error("Current queue entry is missing");
        };
        if position > 0 {
            return self.activate(self.state.play_order[position - 1], status.clone());
        }
        if self.state.repeat_mode == RepeatMode::All {
            if let Some(last) = self.state.play_order.last().copied() {
                return self.activate(last, status);
            }
        }
        Vec::new()
    }

    fn activate_existing_entry(
        &mut self,
        entry_id: QueueEntryId,
        status: PlaybackStatus,
    ) -> Vec<PlayerEvent> {
        if !self.state.queue.iter().any(|entry| entry.id == entry_id) {
            return self.error("Queue entry is missing");
        }
        self.activate(entry_id, status)
    }

    fn seek(&mut self, seconds: f32) -> Vec<PlayerEvent> {
        let duration = self.state.duration_seconds.unwrap_or(0.0);
        self.state.position_seconds = if duration > 0.0 {
            seconds.clamp(0.0, duration)
        } else {
            seconds.max(0.0)
        };
        vec![PlayerEvent::PositionChanged {
            seconds: self.state.position_seconds,
        }]
    }

    fn set_shuffle(&mut self, enabled: bool) -> Vec<PlayerEvent> {
        if self.state.shuffle_enabled == enabled {
            return Vec::new();
        }
        self.state.shuffle_enabled = enabled;
        self.rebuild_play_order(self.state.current_queue_entry_id);
        vec![PlayerEvent::QueueChanged]
    }

    fn add_queue_entry(&mut self, track_id: TrackId) -> Vec<PlayerEvent> {
        if !self.track_metadata.contains_key(&track_id) {
            return self.error(format!("Unknown track {}", track_id.get()));
        }
        let entry = QueueEntry {
            id: self.allocate_entry_id(),
            track_id,
        };
        self.state.queue.push(entry);
        if self.state.shuffle_enabled {
            self.state.play_order.push(entry.id);
        } else {
            self.rebuild_play_order(self.state.current_queue_entry_id);
        }
        vec![PlayerEvent::QueueChanged]
    }

    fn remove_queue_entry(&mut self, entry_id: QueueEntryId) -> Vec<PlayerEvent> {
        let Some(index) = self
            .state
            .queue
            .iter()
            .position(|entry| entry.id == entry_id)
        else {
            return self.error("Queue entry does not exist");
        };
        let was_current = self.state.current_queue_entry_id == Some(entry_id);
        let previous_status = self.state.playback_status.clone();
        let previous_order = self.state.play_order.clone();
        let previous_position = previous_order.iter().position(|id| *id == entry_id);

        self.state.queue.remove(index);
        self.state.play_order.retain(|id| *id != entry_id);
        let mut events = vec![PlayerEvent::QueueChanged];

        if self.state.queue.is_empty() {
            self.state.current_track_id = None;
            self.state.current_queue_entry_id = None;
            self.state.playback_status = PlaybackStatus::Idle;
            self.state.position_seconds = 0.0;
            self.state.duration_seconds = None;
            return events;
        }

        if was_current {
            let replacement = previous_position
                .and_then(|position| previous_order.get(position + 1).copied())
                .filter(|id| self.entry(*id).is_some())
                .or_else(|| self.state.play_order.last().copied());
            if let Some(replacement) = replacement {
                let target_status = if matches!(previous_status, PlaybackStatus::Paused) {
                    PlaybackStatus::Paused
                } else {
                    PlaybackStatus::Playing
                };
                events.extend(self.activate(replacement, target_status));
            }
        }
        events
    }

    fn move_queue_entry(&mut self, entry_id: QueueEntryId, to_index: usize) -> Vec<PlayerEvent> {
        let Some(from_index) = self
            .state
            .queue
            .iter()
            .position(|entry| entry.id == entry_id)
        else {
            return self.error("Queue entry does not exist");
        };
        let entry = self.state.queue.remove(from_index);
        let destination = to_index.min(self.state.queue.len());
        self.state.queue.insert(destination, entry);
        if !self.state.shuffle_enabled {
            self.rebuild_play_order(self.state.current_queue_entry_id);
        }
        vec![PlayerEvent::QueueChanged]
    }

    fn playback_finished(&mut self) -> Vec<PlayerEvent> {
        let Some(entry_id) = self.state.current_queue_entry_id else {
            return Vec::new();
        };
        let Some(track_id) = self.state.current_track_id else {
            return self.error("Current queue entry has no track");
        };
        let mut events = vec![PlayerEvent::TrackEnded { track_id, entry_id }];
        match self.state.repeat_mode {
            RepeatMode::One => events.extend(self.activate(entry_id, PlaybackStatus::Playing)),
            RepeatMode::All => {
                if let Some(next) = self
                    .next_after(entry_id)
                    .or_else(|| self.state.play_order.first().copied())
                {
                    events.extend(self.activate(next, PlaybackStatus::Playing));
                }
            }
            RepeatMode::Off => {
                if let Some(next) = self.next_after(entry_id) {
                    events.extend(self.activate(next, PlaybackStatus::Playing));
                } else {
                    self.state.playback_status = PlaybackStatus::Ended;
                    self.state.position_seconds = self.state.duration_seconds.unwrap_or(0.0);
                }
            }
        }
        events
    }

    fn activate(
        &mut self,
        entry_id: QueueEntryId,
        desired_status: PlaybackStatus,
    ) -> Vec<PlayerEvent> {
        let Some(entry) = self.entry(entry_id) else {
            return self.error("Queue entry does not exist");
        };
        let Some(track) = self.track_metadata.get(&entry.track_id).copied() else {
            return self.error("Queue entry references an unknown track");
        };
        if track.availability != PlaybackAvailability::Available {
            return self.error("This preview track is unavailable for playback");
        }

        self.state.current_queue_entry_id = Some(entry.id);
        self.state.current_track_id = Some(entry.track_id);
        self.state.position_seconds = 0.0;
        self.state.duration_seconds = track.duration_seconds.map(|seconds| seconds as f32);
        self.state.playback_status = PlaybackStatus::Loading;

        let mut events = vec![PlayerEvent::TrackLoading {
            track_id: entry.track_id,
            entry_id: entry.id,
        }];
        self.state.playback_status = desired_status.clone();
        match desired_status {
            PlaybackStatus::Paused => events.push(PlayerEvent::TrackPaused {
                track_id: entry.track_id,
                entry_id: entry.id,
            }),
            PlaybackStatus::Playing
            | PlaybackStatus::Loading
            | PlaybackStatus::Ended
            | PlaybackStatus::Idle
            | PlaybackStatus::Error(_) => {
                events.push(PlayerEvent::TrackPlaying {
                    track_id: entry.track_id,
                    entry_id: entry.id,
                });
            }
        }
        events
    }

    fn rebuild_play_order(&mut self, current: Option<QueueEntryId>) {
        let mut order: Vec<_> = self.state.queue.iter().map(|entry| entry.id).collect();
        if self.state.shuffle_enabled && order.len() > 1 {
            let current = current.filter(|id| order.contains(id));
            order.retain(|id| Some(*id) != current);
            self.shuffle(&mut order);
            if let Some(current) = current {
                order.insert(0, current);
            }
        }
        self.state.play_order = order;
    }

    fn shuffle(&mut self, entries: &mut [QueueEntryId]) {
        for index in (1..entries.len()).rev() {
            let other = (self.next_random() as usize) % (index + 1);
            entries.swap(index, other);
        }
    }

    fn next_random(&mut self) -> u64 {
        self.shuffle_seed ^= self.shuffle_seed << 7;
        self.shuffle_seed ^= self.shuffle_seed >> 9;
        self.shuffle_seed ^= self.shuffle_seed << 8;
        self.shuffle_seed
    }

    fn allocate_entry_id(&mut self) -> QueueEntryId {
        let id = QueueEntryId(self.next_queue_entry_id);
        self.next_queue_entry_id = self.next_queue_entry_id.saturating_add(1);
        id
    }

    fn entry(&self, id: QueueEntryId) -> Option<QueueEntry> {
        self.state
            .queue
            .iter()
            .copied()
            .find(|entry| entry.id == id)
    }

    fn play_order_index(&self, id: QueueEntryId) -> Option<usize> {
        self.state
            .play_order
            .iter()
            .position(|candidate| *candidate == id)
    }

    fn next_after(&self, id: QueueEntryId) -> Option<QueueEntryId> {
        self.play_order_index(id)
            .and_then(|position| self.state.play_order.get(position + 1).copied())
    }

    fn error(&mut self, message: impl Into<String>) -> Vec<PlayerEvent> {
        let message = message.into();
        self.state.playback_status = PlaybackStatus::Error(message.clone());
        vec![PlayerEvent::PlaybackError { message }]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::UiState;

    fn preview_controller() -> PlayerController {
        let state = UiState::default();
        let ids = state.home_track_ids();
        let mut controller = PlayerController::new();
        controller.sync_track_metadata(ids.iter().filter_map(|id| {
            state.track(*id).map(|track| {
                (
                    *id,
                    TrackPlaybackMetadata {
                        duration_seconds: track.duration_seconds,
                        availability: track.availability,
                    },
                )
            })
        }));
        controller
    }

    fn preview_ids() -> Vec<TrackId> {
        (0..10).map(|index| TrackId::new(10_001 + index)).collect()
    }
    fn preview_id(index: usize) -> TrackId {
        preview_ids()[index]
    }

    fn play_full_preview(controller: &mut PlayerController, start_at: usize) {
        controller.dispatch(PlayerCommand::PlayCollection {
            tracks: preview_ids(),
            start_at,
        });
    }

    #[test]
    fn fifth_track_next_advances_to_sixth_track() {
        let mut controller = preview_controller();
        play_full_preview(&mut controller, 4);
        assert_eq!(controller.state().current_track_id, Some(preview_id(4)));
        controller.dispatch(PlayerCommand::Next);
        assert_eq!(controller.state().current_track_id, Some(preview_id(5)));
    }

    #[test]
    fn previous_returns_to_the_prior_entry() {
        let mut controller = preview_controller();
        play_full_preview(&mut controller, 4);
        controller.dispatch(PlayerCommand::Previous);
        assert_eq!(controller.state().current_track_id, Some(preview_id(3)));
    }

    #[test]
    fn play_pause_is_reflected_by_one_status() {
        let mut controller = preview_controller();
        play_full_preview(&mut controller, 0);
        controller.dispatch(PlayerCommand::TogglePlayPause);
        assert_eq!(controller.state().playback_status, PlaybackStatus::Paused);
        controller.dispatch(PlayerCommand::TogglePlayPause);
        assert_eq!(controller.state().playback_status, PlaybackStatus::Playing);
    }

    #[test]
    fn stop_preserves_queue_and_resets_transport() {
        let mut controller = preview_controller();
        play_full_preview(&mut controller, 4);
        controller.dispatch(PlayerCommand::Seek { seconds: 42.0 });
        let selected = controller.state().current_queue_entry_id;
        let events = controller.dispatch(PlayerCommand::Stop);
        assert_eq!(controller.state().playback_status, PlaybackStatus::Idle);
        assert_eq!(controller.state().position_seconds, 0.0);
        assert_eq!(controller.state().current_track_id, Some(preview_id(4)));
        assert_eq!(controller.state().current_queue_entry_id, selected);
        assert_eq!(controller.state().queue.len(), 10);
        assert!(matches!(
            events.as_slice(),
            [PlayerEvent::TrackStopped { .. }]
        ));
    }

    #[test]
    fn metadata_selection_builds_a_queue_without_claiming_playback() {
        let mut controller = preview_controller();
        let events = controller.dispatch(PlayerCommand::SelectTrack {
            track_id: preview_id(4),
            context: preview_ids(),
        });
        assert_eq!(controller.state().current_track_id, Some(preview_id(4)));
        assert_eq!(controller.state().queue.len(), 10);
        assert_eq!(controller.state().playback_status, PlaybackStatus::Paused);
        assert!(matches!(
            events.as_slice(),
            [
                PlayerEvent::QueueChanged,
                PlayerEvent::TrackLoading { .. },
                PlayerEvent::TrackPaused { .. }
            ]
        ));
    }

    #[test]
    fn repeat_modes_have_explicit_end_of_queue_rules() {
        let mut controller = preview_controller();
        play_full_preview(&mut controller, preview_ids().len() - 1);
        controller.dispatch(PlayerCommand::SetRepeat(RepeatMode::Off));
        controller.dispatch(PlayerCommand::PlaybackFinished);
        assert_eq!(controller.state().playback_status, PlaybackStatus::Ended);

        play_full_preview(&mut controller, preview_ids().len() - 1);
        controller.dispatch(PlayerCommand::SetRepeat(RepeatMode::All));
        controller.dispatch(PlayerCommand::PlaybackFinished);
        assert_eq!(controller.state().current_track_id, Some(preview_id(0)));

        play_full_preview(&mut controller, 4);
        let selected = controller.state().current_queue_entry_id;
        controller.dispatch(PlayerCommand::SetRepeat(RepeatMode::One));
        controller.dispatch(PlayerCommand::PlaybackFinished);
        assert_eq!(controller.state().current_queue_entry_id, selected);
        assert_eq!(controller.state().playback_status, PlaybackStatus::Playing);
    }

    #[test]
    fn shuffle_order_is_stable_until_a_new_command_changes_it() {
        let mut controller = preview_controller();
        play_full_preview(&mut controller, 0);
        controller.dispatch(PlayerCommand::SetShuffle(true));
        let order = controller.state().play_order().to_vec();
        assert_eq!(
            order.first(),
            controller.state().current_queue_entry_id.as_ref()
        );
        controller.dispatch(PlayerCommand::Next);
        assert_eq!(controller.state().play_order(), order.as_slice());
        assert_ne!(controller.state().current_track_id, Some(preview_id(0)));
    }

    #[test]
    fn duplicate_tracks_get_distinct_queue_entry_ids() {
        let mut controller = preview_controller();
        controller.dispatch(PlayerCommand::PlayCollection {
            tracks: vec![preview_id(0), preview_id(0)],
            start_at: 0,
        });
        assert_ne!(
            controller.state().queue[0].id,
            controller.state().queue[1].id
        );
        let first_entry = controller.state().current_queue_entry_id;
        controller.dispatch(PlayerCommand::Next);
        assert_ne!(controller.state().current_queue_entry_id, first_entry);
        assert_eq!(controller.state().current_track_id, Some(preview_id(0)));
    }

    #[test]
    fn metadata_navigation_and_queue_selection_preserve_duplicate_entry_identity() {
        let mut controller = preview_controller();
        controller.dispatch(PlayerCommand::SelectCollection {
            tracks: vec![preview_id(0), preview_id(1), preview_id(0)],
            start_at: 2,
        });
        let selected = controller.state().queue[2].id;
        assert_eq!(controller.state().current_queue_entry_id, Some(selected));
        assert_eq!(controller.state().playback_status, PlaybackStatus::Paused);

        let first_duplicate = controller.state().queue[0].id;
        controller.dispatch(PlayerCommand::SelectQueueEntry {
            entry_id: first_duplicate,
        });
        assert_eq!(
            controller.state().current_queue_entry_id,
            Some(first_duplicate)
        );
        assert_eq!(controller.state().playback_status, PlaybackStatus::Paused);
        controller.dispatch(PlayerCommand::SelectNext);
        assert_eq!(controller.state().current_track_id, Some(preview_id(1)));
        assert_eq!(controller.state().playback_status, PlaybackStatus::Paused);
    }

    #[test]
    fn queue_entries_can_move_and_current_entry_can_be_removed() {
        let mut controller = preview_controller();
        play_full_preview(&mut controller, 1);
        let moved = controller.state().queue[8].id;
        controller.dispatch(PlayerCommand::MoveQueueEntry {
            entry_id: moved,
            to_index: 2,
        });
        assert_eq!(controller.state().queue[2].id, moved);

        let current = controller.state().current_queue_entry_id.unwrap();
        controller.dispatch(PlayerCommand::RemoveQueueEntry { entry_id: current });
        assert_ne!(controller.state().current_queue_entry_id, Some(current));
        assert!(
            !controller
                .state()
                .queue
                .iter()
                .any(|entry| entry.id == current)
        );
    }

    #[test]
    fn queue_can_be_empty_or_grow_without_using_track_ids_as_entry_ids() {
        let mut controller = preview_controller();
        let first = preview_id(0);
        let events = controller.dispatch(PlayerCommand::Next);
        assert!(matches!(
            events.as_slice(),
            [PlayerEvent::PlaybackError { .. }]
        ));

        controller.dispatch(PlayerCommand::AddQueueEntry { track_id: first });
        controller.dispatch(PlayerCommand::AddQueueEntry { track_id: first });
        assert_eq!(controller.state().queue.len(), 2);
        assert_ne!(
            controller.state().queue[0].id,
            controller.state().queue[1].id
        );
    }

    #[test]
    fn seek_is_bounded_by_known_duration() {
        let mut controller = preview_controller();
        play_full_preview(&mut controller, 0);
        controller.dispatch(PlayerCommand::Seek { seconds: -5.0 });
        assert_eq!(controller.state().position_seconds, 0.0);
        controller.dispatch(PlayerCommand::Seek { seconds: 99_999.0 });
        assert_eq!(controller.state().position_seconds, 222.0);
    }

    #[test]
    fn unknown_duration_accepts_a_nonnegative_seek_without_panicking() {
        let mut controller = preview_controller();
        play_full_preview(&mut controller, 0);
        controller.state.duration_seconds = None;
        controller.dispatch(PlayerCommand::Seek { seconds: 123.5 });
        assert_eq!(controller.state().position_seconds, 123.5);
    }
}
