//! RC003 TV 键的原生 `VK_OEM_3` 延迟判定。
//!
//! 遥控器把 TV 键上报为 HID usage 0x35，Windows 同时把它翻译成反引号。
//! 低级键盘钩子没有设备句柄，不能立即区分遥控器和实体键盘，因此先吞掉
//! 候选事件，等待本地 HID Tap 的 TV 信号；未确认的事件会由调用方回放。

use parking_lot::Mutex;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

pub const TV_CORRELATION_WINDOW: Duration = Duration::from_millis(120);
const MAX_PENDING_EVENTS: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TvNativeEvent {
    pub down: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub enum CandidateAction {
    /// 吞掉候选键，并在给定 generation 上安排一次超时检查。
    Swallow { deadline_generation: Option<u64> },
    /// 吞掉候选键并安全回放已缓存事件（队列满时 fail-open）。
    ReplayAndSwallow(Vec<TvNativeEvent>),
    /// 已确认是实体键盘，直接放行后续事件。
    PassThrough,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirectSignalOutcome {
    pub dropped_events: usize,
    pub elapsed: Option<Duration>,
    pub release_deadline_generation: Option<u64>,
}

#[derive(Debug)]
enum State {
    Idle,
    Pending {
        generation: u64,
        started: Instant,
        events: Vec<TvNativeEvent>,
    },
    /// 已确认是遥控器 TV，等待原生键的配对 UP。
    Suppressing,
    /// 已收到遥控器 UP；仍给 Windows 对应原生 UP 一个短尾窗，避免粘键。
    Releasing {
        generation: u64,
    },
    /// 候选键已经回放；其余 typematic/UP 让 Windows 原样处理。
    Passthrough,
}

#[derive(Debug)]
pub struct TvNativeGuard {
    state: State,
    next_generation: u64,
}

impl Default for TvNativeGuard {
    fn default() -> Self {
        Self {
            state: State::Idle,
            next_generation: 0,
        }
    }
}

impl TvNativeGuard {
    pub fn on_candidate(&mut self, event: TvNativeEvent) -> CandidateAction {
        match &mut self.state {
            State::Idle => {
                self.next_generation = self.next_generation.wrapping_add(1);
                let generation = self.next_generation;
                self.state = State::Pending {
                    generation,
                    started: Instant::now(),
                    events: vec![event],
                };
                CandidateAction::Swallow {
                    deadline_generation: Some(generation),
                }
            }
            State::Pending { events, .. } => {
                if events.len() < MAX_PENDING_EVENTS {
                    events.push(event);
                    CandidateAction::Swallow {
                        deadline_generation: None,
                    }
                } else {
                    let events = match std::mem::replace(&mut self.state, State::Passthrough) {
                        State::Pending { mut events, .. } => {
                            events.push(event);
                            events
                        }
                        _ => unreachable!("state was pending before replacement"),
                    };
                    CandidateAction::ReplayAndSwallow(events)
                }
            }
            State::Suppressing | State::Releasing { .. } => {
                if !event.down {
                    self.state = State::Idle;
                }
                CandidateAction::Swallow {
                    deadline_generation: None,
                }
            }
            State::Passthrough => {
                if !event.down {
                    self.state = State::Idle;
                }
                CandidateAction::PassThrough
            }
        }
    }

    /// HID Tap 在配置、TV gate 和 `KeyAction::None` 之前调用。
    pub fn on_remote_tv_signal(&mut self, pressed: bool) -> DirectSignalOutcome {
        if !pressed {
            if matches!(self.state, State::Suppressing) {
                self.next_generation = self.next_generation.wrapping_add(1);
                let generation = self.next_generation;
                self.state = State::Releasing { generation };
                return DirectSignalOutcome {
                    dropped_events: 0,
                    elapsed: None,
                    release_deadline_generation: Some(generation),
                };
            }
            return DirectSignalOutcome {
                dropped_events: 0,
                elapsed: None,
                release_deadline_generation: None,
            };
        }

        match std::mem::replace(&mut self.state, State::Suppressing) {
            State::Pending {
                started, events, ..
            } => {
                let has_up = events.iter().any(|event| !event.down);
                if has_up {
                    self.state = State::Idle;
                }
                DirectSignalOutcome {
                    dropped_events: events.len(),
                    elapsed: Some(started.elapsed()),
                    release_deadline_generation: None,
                }
            }
            State::Passthrough => {
                // 超时后已经把实体键回放，迟到的遥控器信号不能反过来吞键。
                self.state = State::Passthrough;
                DirectSignalOutcome {
                    dropped_events: 0,
                    elapsed: None,
                    release_deadline_generation: None,
                }
            }
            State::Idle | State::Suppressing | State::Releasing { .. } => DirectSignalOutcome {
                dropped_events: 0,
                elapsed: None,
                release_deadline_generation: None,
            },
        }
    }

    pub fn on_deadline(&mut self, generation: u64) -> Option<Vec<TvNativeEvent>> {
        match &self.state {
            State::Pending {
                generation: pending_generation,
                ..
            } if *pending_generation == generation => {
                let events = match std::mem::replace(&mut self.state, State::Idle) {
                    State::Pending { events, .. } => events,
                    _ => unreachable!("matching pending generation must stay pending"),
                };
                if !events.iter().any(|event| !event.down) {
                    self.state = State::Passthrough;
                }
                Some(events)
            }
            State::Releasing {
                generation: release_generation,
            } if *release_generation == generation => {
                self.state = State::Idle;
                None
            }
            _ => None,
        }
    }

    pub fn reset(&mut self) -> Vec<TvNativeEvent> {
        match std::mem::replace(&mut self.state, State::Idle) {
            State::Pending { events, .. } => events,
            State::Idle | State::Suppressing | State::Releasing { .. } | State::Passthrough => {
                Vec::new()
            }
        }
    }
}

fn global_guard() -> &'static Mutex<TvNativeGuard> {
    static GUARD: OnceLock<Mutex<TvNativeGuard>> = OnceLock::new();
    GUARD.get_or_init(|| Mutex::new(TvNativeGuard::default()))
}

pub fn on_native_candidate(down: bool) -> CandidateAction {
    global_guard().lock().on_candidate(TvNativeEvent { down })
}

pub fn on_remote_tv_signal(pressed: bool) -> DirectSignalOutcome {
    global_guard().lock().on_remote_tv_signal(pressed)
}

pub fn on_deadline(generation: u64) -> Option<Vec<TvNativeEvent>> {
    global_guard().lock().on_deadline(generation)
}

pub fn reset() -> Vec<TvNativeEvent> {
    global_guard().lock().reset()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn down() -> TvNativeEvent {
        TvNativeEvent { down: true }
    }

    fn up() -> TvNativeEvent {
        TvNativeEvent { down: false }
    }

    #[test]
    fn remote_signal_drops_pending_down_and_up() {
        let mut guard = TvNativeGuard::default();
        let CandidateAction::Swallow {
            deadline_generation: Some(generation),
        } = guard.on_candidate(down())
        else {
            panic!("first candidate must start a deadline");
        };
        assert!(matches!(
            guard.on_candidate(up()),
            CandidateAction::Swallow { .. }
        ));

        let outcome = guard.on_remote_tv_signal(true);
        assert_eq!(outcome.dropped_events, 2);
        assert!(guard.on_deadline(generation).is_none());
    }

    #[test]
    fn timeout_replays_short_physical_key_pair_once() {
        let mut guard = TvNativeGuard::default();
        let CandidateAction::Swallow {
            deadline_generation: Some(generation),
        } = guard.on_candidate(down())
        else {
            panic!("first candidate must start a deadline");
        };
        let _ = guard.on_candidate(up());
        assert_eq!(guard.on_deadline(generation), Some(vec![down(), up()]));
        assert!(matches!(
            guard.on_candidate(down()),
            CandidateAction::Swallow { .. }
        ));
    }

    #[test]
    fn remote_signal_before_native_event_suppresses_the_pair() {
        let mut guard = TvNativeGuard::default();
        assert_eq!(guard.on_remote_tv_signal(true).dropped_events, 0);
        assert!(matches!(
            guard.on_candidate(down()),
            CandidateAction::Swallow { .. }
        ));
        assert!(matches!(
            guard.on_candidate(up()),
            CandidateAction::Swallow { .. }
        ));
        assert!(matches!(
            guard.on_candidate(down()),
            CandidateAction::Swallow {
                deadline_generation: Some(_)
            }
        ));
    }

    #[test]
    fn remote_release_expires_when_no_native_key_arrives() {
        let mut guard = TvNativeGuard::default();
        let _ = guard.on_remote_tv_signal(true);
        let outcome = guard.on_remote_tv_signal(false);
        let generation = outcome
            .release_deadline_generation
            .expect("remote UP must start a bounded cleanup tail");
        assert!(guard.on_deadline(generation).is_none());
        assert!(matches!(
            guard.on_candidate(down()),
            CandidateAction::Swallow {
                deadline_generation: Some(_)
            }
        ));
    }

    #[test]
    fn late_remote_signal_cannot_retroactively_drop_replayed_key() {
        let mut guard = TvNativeGuard::default();
        let CandidateAction::Swallow {
            deadline_generation: Some(generation),
        } = guard.on_candidate(down())
        else {
            panic!("first candidate must start a deadline");
        };
        assert_eq!(guard.on_deadline(generation), Some(vec![down()]));
        assert_eq!(guard.on_remote_tv_signal(true).dropped_events, 0);
        assert_eq!(guard.on_candidate(up()), CandidateAction::PassThrough);
    }

    #[test]
    fn queue_overflow_fails_open_by_replaying_all_buffered_events() {
        let mut guard = TvNativeGuard::default();
        let _ = guard.on_candidate(down());
        for _ in 1..MAX_PENDING_EVENTS {
            assert!(matches!(
                guard.on_candidate(down()),
                CandidateAction::Swallow { .. }
            ));
        }
        let CandidateAction::ReplayAndSwallow(events) = guard.on_candidate(down()) else {
            panic!("overflow must replay rather than lose a physical key");
        };
        assert_eq!(events.len(), MAX_PENDING_EVENTS + 1);
        assert_eq!(guard.on_candidate(up()), CandidateAction::PassThrough);
    }

    #[test]
    fn reset_returns_pending_events_for_safe_replay() {
        let mut guard = TvNativeGuard::default();
        let _ = guard.on_candidate(down());
        assert_eq!(guard.reset(), vec![down()]);
        assert!(matches!(
            guard.on_candidate(up()),
            CandidateAction::Swallow { .. }
        ));
    }
}
