//! A key owns only the session it started. Input timestamps discard late events
//! from independently scheduled launchers; key repeats never toggle twice.
use dicta_core::{Command, ShortcutMode};

#[derive(Default)]
pub(crate) struct ShortcutState {
    latest: u64,
    latest_token: u64,
    down: bool,
    owner: Option<u64>,
}
impl ShortcutState {
    pub fn press(
        &mut self,
        at: u64,
        token: Option<u64>,
        mode: ShortcutMode,
        busy: bool,
        next_session: u64,
    ) -> Option<Command> {
        if let Some(token) = token {
            if token < self.latest_token || (token == self.latest_token && !self.down) {
                return None;
            }
            if token > self.latest_token {
                self.latest_token = token;
                // A new gesture can recover a lost release. End the old hold
                // rather than transferring microphone ownership silently.
                if self.down
                    && self
                        .owner
                        .take()
                        .is_some_and(|id| busy && id + 1 == next_session)
                {
                    self.latest = self.latest.max(at);
                    return Some(Command::Stop);
                }
                self.down = false;
            }
        }
        if token.is_none() && at <= self.latest {
            return None;
        }
        self.latest = self.latest.max(at);
        if self.down {
            return None;
        }
        self.down = true;
        match mode {
            ShortcutMode::Toggle => Some(Command::Toggle),
            ShortcutMode::Hold if !busy => {
                self.owner = Some(next_session);
                Some(Command::Start)
            }
            ShortcutMode::Hold => None,
        }
    }
    pub fn release(&mut self, at: u64, token: Option<u64>, active: Option<u64>) -> Option<Command> {
        if let Some(token) = token {
            if token < self.latest_token {
                return None;
            }
            self.latest_token = token;
        }
        if token.is_none() && at <= self.latest {
            return None;
        }
        self.latest = self.latest.max(at);
        self.down = false;
        self.finish_owned(active)
    }
    pub fn finish_owned(&mut self, active: Option<u64>) -> Option<Command> {
        let owner = self.owner.take();
        if owner.is_some() && owner == active {
            Some(Command::Stop)
        } else {
            None
        }
    }
    pub fn detach(&mut self) {
        self.owner = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hold_repeat_release_and_cancel_preserve_session_ownership() {
        let mut state = ShortcutState::default();
        assert!(matches!(
            state.press(1, None, ShortcutMode::Hold, false, 1),
            Some(Command::Start)
        ));
        assert!(state.press(2, None, ShortcutMode::Hold, true, 2).is_none());
        assert!(matches!(
            state.release(3, None, Some(1)),
            Some(Command::Stop)
        ));
        assert!(state.release(4, None, Some(1)).is_none());
        assert!(state.press(5, None, ShortcutMode::Hold, true, 2).is_none());
        assert!(
            state.release(6, None, Some(1)).is_none(),
            "Hold must not stop a UI-started session"
        );
        state.press(7, None, ShortcutMode::Hold, false, 2);
        state.detach();
        assert!(
            state.press(8, None, ShortcutMode::Hold, false, 3).is_none(),
            "cancel while down must not restart"
        );
        assert!(state.release(9, None, Some(3)).is_none());
    }
    #[test]
    fn token_rejects_press_process_that_started_after_its_release() {
        let mut state = ShortcutState::default();
        assert!(state.release(10, Some(100), None).is_none());
        assert!(
            state
                .press(20, Some(100), ShortcutMode::Hold, false, 1)
                .is_none()
        );
        assert!(matches!(
            state.press(30, Some(101), ShortcutMode::Hold, false, 1),
            Some(Command::Start)
        ));
        assert!(state.release(40, Some(100), Some(1)).is_none());
        assert!(matches!(
            state.release(50, Some(101), Some(1)),
            Some(Command::Stop)
        ));
        assert!(matches!(
            state.press(70, Some(102), ShortcutMode::Hold, false, 2),
            Some(Command::Start)
        ));
        // The release process may run first, yet lose the IPC lock to press.
        assert!(matches!(
            state.release(60, Some(102), Some(2)),
            Some(Command::Stop)
        ));
        assert!(
            state
                .press(80, Some(102), ShortcutMode::Hold, false, 3)
                .is_none()
        );
    }
    #[test]
    fn delayed_press_after_release_cannot_open_microphone() {
        let mut state = ShortcutState::default();
        assert!(state.release(20, None, None).is_none());
        assert!(
            state
                .press(10, None, ShortcutMode::Hold, false, 1)
                .is_none()
        );
        assert!(matches!(
            state.press(30, None, ShortcutMode::Hold, false, 1),
            Some(Command::Start)
        ));
    }
    #[test]
    fn toggling_requires_a_new_down_edge_and_mode_flip_finishes_hold() {
        let mut state = ShortcutState::default();
        assert!(matches!(
            state.press(1, None, ShortcutMode::Toggle, false, 1),
            Some(Command::Toggle)
        ));
        assert!(
            state
                .press(2, None, ShortcutMode::Toggle, true, 2)
                .is_none()
        );
        assert!(state.release(3, None, Some(1)).is_none());
        assert!(matches!(
            state.press(4, None, ShortcutMode::Toggle, true, 2),
            Some(Command::Toggle)
        ));
        state.release(5, None, None);
        state.press(6, None, ShortcutMode::Hold, false, 2);
        assert!(matches!(state.finish_owned(Some(2)), Some(Command::Stop)));
        assert!(
            state
                .press(7, None, ShortcutMode::Toggle, true, 3)
                .is_none()
        );
        assert!(state.release(8, None, Some(2)).is_none());
    }
}
