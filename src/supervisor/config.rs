use std::time::Duration;

#[derive(Debug, Clone, Copy)]
pub enum Strategy {
    OneForOne,
    OneForAll,
}

#[derive(Debug, Clone, Copy, Default)]
pub enum ChildRestart {
    Permanent,
    #[default]
    Transient,
    Temporary,
}

impl ChildRestart {
    pub fn should_restart(&self, is_failure: bool) -> bool {
        match self {
            Self::Permanent => true,
            Self::Transient => is_failure,
            Self::Temporary => false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ShutdownPolicy {
    pub grace: Duration,
}

impl Default for ShutdownPolicy {
    fn default() -> Self {
        Self {
            grace: Duration::from_secs(5),
        }
    }
}

#[derive(Debug, Clone)]
pub struct RestartIntensity {
    pub max_restarts: u32,
    pub window: Duration,
}

impl Default for RestartIntensity {
    fn default() -> Self {
        Self {
            max_restarts: 10,
            window: Duration::from_secs(60),
        }
    }
}

#[derive(Debug, Clone)]
pub struct SupervisorConfig {
    pub strategy: Strategy,
    pub intensity: RestartIntensity,
    pub hang_check_interval: Duration,
    pub event_capacity: usize,
}

impl Default for SupervisorConfig {
    fn default() -> Self {
        Self {
            strategy: Strategy::OneForOne,
            intensity: RestartIntensity::default(),
            hang_check_interval: Duration::from_secs(1),
            event_capacity: 64,
        }
    }
}
