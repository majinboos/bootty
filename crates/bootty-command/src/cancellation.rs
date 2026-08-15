use std::sync::{
    Arc,
    atomic::{AtomicU8, Ordering},
};

#[derive(Clone, Debug, Default)]
pub struct CommandCancellation(Arc<AtomicU8>);

impl CommandCancellation {
    const PENDING: u8 = 0;
    const STARTED: u8 = 1;
    const CANCELLED: u8 = 2;

    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) -> bool {
        self.0
            .compare_exchange(
                Self::PENDING,
                Self::CANCELLED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire) == Self::CANCELLED
    }

    pub fn try_start(&self) -> bool {
        self.0
            .compare_exchange(
                Self::PENDING,
                Self::STARTED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }
}
