//! Runtime state types: environment, lifecycle, and availability.

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

/// Runtime environment, normalized from `RUME_ENV`/`NODE_ENV`.
#[derive(Debug, Clone)]
pub struct Environment {
    name: String,
}

impl Environment {
    pub fn from_env() -> Self {
        Self {
            name: crate::env::resolve_env_name(),
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn in_development(&self) -> bool {
        matches!(self.name.as_str(), "development" | "dev" | "develop")
    }

    pub fn in_production(&self) -> bool {
        matches!(self.name.as_str(), "production" | "prod")
    }

    pub fn in_test(&self) -> bool {
        matches!(self.name.as_str(), "test" | "testing")
    }
}

/// How far the application intends to go during startup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplicationMode {
    /// Boot and serve traffic.
    Run,
    /// Boot for inspection/tooling without serving or side effects.
    Inspect,
}

/// Lifecycle state of a running application.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum LifecycleState {
    Created = 0,
    Booted = 1,
    Ready = 2,
    Terminating = 3,
    Terminated = 4,
}

/// A shared lifecycle handle.
#[derive(Clone, Debug)]
pub struct Lifecycle {
    inner: Arc<AtomicU8>,
}

impl Lifecycle {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(AtomicU8::new(LifecycleState::Created as u8)),
        }
    }
}

impl Default for Lifecycle {
    fn default() -> Self {
        Self::new()
    }
}

impl Lifecycle {
    /// The highest valid on-disc value for `LifecycleState` (`Terminated = 4`).
    const MAX_DISCRIMINANT: u8 = LifecycleState::Terminated as u8;

    pub fn state(&self) -> LifecycleState {
        match self.inner.load(Ordering::SeqCst) {
            0 => LifecycleState::Created,
            1 => LifecycleState::Booted,
            2 => LifecycleState::Ready,
            3 => LifecycleState::Terminating,
            4 => LifecycleState::Terminated,
            // Any stray u8 (e.g. 99, 255) falls back to `Created` instead of
            // pretending to be a real lifecycle state.
            _ => LifecycleState::Created,
        }
    }

    pub(crate) fn set(&self, state: LifecycleState) {
        // The type system guarantees only valid variants reach here through
        // safe code, but guard the stored discriminant anyway so an out-of-range
        // u8 can never be observed through `state()`.
        let value = state as u8;
        if value <= Self::MAX_DISCRIMINANT {
            self.inner.store(value, Ordering::SeqCst);
        }
    }

    pub fn is_ready(&self) -> bool {
        self.state() == LifecycleState::Ready
    }

    pub fn is_terminated(&self) -> bool {
        self.state() == LifecycleState::Terminated
    }
}

/// Availability state for health/readiness reporting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum AvailabilityState {
    Unknown = 0,
    Live = 1,
    Ready = 2,
    RefusingTraffic = 3,
    Broken = 4,
}

/// A shared availability handle.
#[derive(Clone, Debug)]
pub struct Availability {
    inner: Arc<AtomicU8>,
}

impl Availability {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(AtomicU8::new(AvailabilityState::Unknown as u8)),
        }
    }
}

impl Default for Availability {
    fn default() -> Self {
        Self::new()
    }
}

impl Availability {
    /// The highest valid on-disc value for `AvailabilityState` (`Broken = 4`).
    const MAX_DISCRIMINANT: u8 = AvailabilityState::Broken as u8;

    pub fn state(&self) -> AvailabilityState {
        match self.inner.load(Ordering::SeqCst) {
            0 => AvailabilityState::Unknown,
            1 => AvailabilityState::Live,
            2 => AvailabilityState::Ready,
            3 => AvailabilityState::RefusingTraffic,
            4 => AvailabilityState::Broken,
            // Any stray u8 maps to `Unknown`.
            _ => AvailabilityState::Unknown,
        }
    }

    pub fn set(&self, state: AvailabilityState) {
        // The type system guarantees only valid variants reach here through
        // safe code, but guard the stored discriminant anyway so an out-of-range
        // u8 can never be observed through `state()`.
        let value = state as u8;
        if value <= Self::MAX_DISCRIMINANT {
            self.inner.store(value, Ordering::SeqCst);
        }
    }

    pub fn is_ready(&self) -> bool {
        self.state() == AvailabilityState::Ready
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn environment_normalizes() {
        // Direct construction avoids mutating process env in tests.
        let env = Environment {
            name: "production".to_string(),
        };
        assert!(env.in_production());
        assert!(!env.in_development());
    }

    #[test]
    fn availability_transitions() {
        let availability = Availability::new();
        assert_eq!(availability.state(), AvailabilityState::Unknown);
        availability.set(AvailabilityState::Ready);
        assert!(availability.is_ready());
    }

    #[test]
    fn lifecycle_transitions() {
        let lifecycle = Lifecycle::new();
        assert_eq!(lifecycle.state(), LifecycleState::Created);
        lifecycle.set(LifecycleState::Ready);
        assert!(lifecycle.is_ready());
    }

    #[test]
    fn lifecycle_invalid_u8_maps_to_created() {
        // C-C3: an out-of-range u8 discriminant must map to Created, never
        // to a real lifecycle state.
        let lifecycle = Lifecycle::new();
        assert_eq!(lifecycle.state(), LifecycleState::Created);

        // Store an invalid u8 directly (simulating memory corruption).
        lifecycle.inner.store(99, Ordering::SeqCst);
        assert_eq!(lifecycle.state(), LifecycleState::Created);

        lifecycle.inner.store(255, Ordering::SeqCst);
        assert_eq!(lifecycle.state(), LifecycleState::Created);
    }

    #[test]
    fn availability_invalid_u8_maps_to_unknown() {
        // C-C3: an out-of-range u8 discriminant must map to Unknown, never
        // to a real availability state.
        let availability = Availability::new();
        assert_eq!(availability.state(), AvailabilityState::Unknown);

        availability.inner.store(99, Ordering::SeqCst);
        assert_eq!(availability.state(), AvailabilityState::Unknown);

        availability.inner.store(255, Ordering::SeqCst);
        assert_eq!(availability.state(), AvailabilityState::Unknown);
    }
}
