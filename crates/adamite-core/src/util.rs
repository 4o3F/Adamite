//! Fallible collection growth, owned slice copies and checked counter updates.

use std::collections::TryReserveError;

pub(crate) trait VecExt<T> {
    fn try_push(&mut self, item: T) -> Result<(), TryReserveError>;
    fn try_reserve(&mut self, additional: usize) -> Result<(), TryReserveError>;
    fn try_reserve_exact(&mut self, additional: usize) -> Result<(), TryReserveError>;
}

impl<T> VecExt<T> for Vec<T> {
    fn try_push(&mut self, item: T) -> Result<(), TryReserveError> {
        VecExt::try_reserve(self, 1)?;
        self.push(item);
        Ok(())
    }

    fn try_reserve(&mut self, additional: usize) -> Result<(), TryReserveError> {
        Vec::try_reserve(self, additional)
    }

    fn try_reserve_exact(&mut self, additional: usize) -> Result<(), TryReserveError> {
        Vec::try_reserve_exact(self, additional)
    }
}

pub(crate) trait SliceExt<T: Copy> {
    fn try_copy(&self) -> Result<Vec<T>, TryReserveError>;
}

impl<T: Copy> SliceExt<T> for [T] {
    fn try_copy(&self) -> Result<Vec<T>, TryReserveError> {
        let mut owned = Vec::new();
        VecExt::try_reserve_exact(&mut owned, self.len())?;
        owned.extend_from_slice(self);
        Ok(owned)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CounterOverflow;

pub(crate) trait CounterExt {
    fn try_bump(&mut self) -> Result<(), CounterOverflow>;
}

impl CounterExt for u64 {
    fn try_bump(&mut self) -> Result<(), CounterOverflow> {
        *self = self.checked_add(1).ok_or(CounterOverflow)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{CounterExt, CounterOverflow, VecExt};

    #[test]
    fn counter_overflow_preserves_value() {
        let mut counter = u64::MAX - 1;
        assert_eq!(counter.try_bump(), Ok(()));
        assert_eq!(counter, u64::MAX);
        assert_eq!(counter.try_bump(), Err(CounterOverflow));
        assert_eq!(counter, u64::MAX);
    }

    #[test]
    fn failed_reservations_preserve_contents() {
        let mut items = vec![1u8, 2, 3];
        assert!(VecExt::try_reserve(&mut items, usize::MAX).is_err());
        assert_eq!(items, [1, 2, 3]);
        assert!(VecExt::try_reserve_exact(&mut items, usize::MAX).is_err());
        assert_eq!(items, [1, 2, 3]);
    }
}
