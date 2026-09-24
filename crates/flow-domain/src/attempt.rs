use std::num::NonZeroU32;

/// A 1-based execution attempt. A pending, never-claimed activity can
/// represent its lack of attempts with `None` rather than attempt zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Attempt(NonZeroU32);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttemptError {
    Zero,
    Overflow,
}

impl Attempt {
    pub fn new(value: u32) -> Result<Self, AttemptError> {
        NonZeroU32::new(value).map(Self).ok_or(AttemptError::Zero)
    }

    pub fn value(self) -> u32 {
        self.0.get()
    }

    pub fn next(self) -> Result<Self, AttemptError> {
        let value = self.value().checked_add(1).ok_or(AttemptError::Overflow)?;
        Self::new(value)
    }
}

#[cfg(test)]
mod tests {
    use super::{Attempt, AttemptError};

    #[test]
    fn attempt_must_start_above_zero() {
        assert_eq!(Attempt::new(0), Err(AttemptError::Zero));
    }

    #[test]
    fn attempt_can_increment() {
        let first = Attempt::new(1).expect("one is a valid attempt number");
        assert_eq!(first.next().expect("second attempt is valid").value(), 2);
    }

    #[test]
    fn increment_reports_overflow() {
        let max = Attempt::new(u32::MAX).expect("nonzero");
        assert_eq!(max.next(), Err(AttemptError::Overflow));
    }
}
