//! Integer token accounting with adapter-owned time.

use std::{
    num::NonZeroU64,
    time::{Duration, Instant},
};

const NANOS_PER_SEC: u128 = 1_000_000_000;

pub(crate) trait Clock {
    fn now(&self) -> Duration;
    fn sleep(&mut self, duration: Duration);
}

pub(crate) struct SystemClock(Instant);

impl Default for SystemClock {
    fn default() -> Self {
        Self(Instant::now())
    }
}

impl Clock for SystemClock {
    fn now(&self) -> Duration {
        self.0.elapsed()
    }

    fn sleep(&mut self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

pub(crate) struct Pacer<C> {
    clock: C,
    rate: Option<NonZeroU64>,
    capacity: u128,
    tokens: u128,
    updated: Duration,
}

impl<C: Clock> Pacer<C> {
    pub(crate) fn new(clock: C, rate: Option<NonZeroU64>, chunk_size: usize) -> Self {
        let updated = clock.now();
        let chunk = u64::try_from(chunk_size).unwrap_or(u64::MAX);
        // A one-millisecond refill window, with space for at least one request.
        let capacity = rate.map_or(chunk, |rate| chunk.max(rate.get() / 1000));
        // Reserve the burst from the one-second budget, so burst + refill <= rate.
        // Below one chunk/second, a single indivisible read can exceed the budget.
        let rate =
            rate.map(|rate| NonZeroU64::new(rate.get().saturating_sub(capacity)).unwrap_or(rate));
        Self {
            clock,
            rate,
            capacity: u128::from(capacity).saturating_mul(NANOS_PER_SEC),
            tokens: 0,
            updated,
        }
    }

    pub(crate) fn acquire(&mut self, bytes: usize) {
        let Some(rate) = self.rate else { return };
        let mut remaining = (bytes as u128).saturating_mul(NANOS_PER_SEC);
        // Charge oversized requests in capacity-sized pieces so they still progress.
        while remaining > 0 {
            let charge = remaining.min(self.capacity);
            loop {
                let now = self.clock.now();
                self.tokens = self
                    .tokens
                    .saturating_add(
                        now.saturating_sub(self.updated)
                            .as_nanos()
                            .saturating_mul(u128::from(rate.get())),
                    )
                    .min(self.capacity);
                self.updated = now;
                if self.tokens >= charge {
                    self.tokens = self.tokens.saturating_sub(charge);
                    break;
                }
                let nanos = charge
                    .saturating_sub(self.tokens)
                    .div_ceil(u128::from(rate.get()));
                let duration = Duration::from_nanos(u64::try_from(nanos).unwrap_or(u64::MAX));
                let span = tracing::info_span!(
                    "rate_limit_wait",
                    bytes,
                    waited_nanos = tracing::field::Empty
                );
                let _entered = span.enter();
                self.clock.sleep(duration);
                span.record(
                    "waited_nanos",
                    u64::try_from(self.clock.now().saturating_sub(now).as_nanos())
                        .unwrap_or(u64::MAX),
                );
            }
            remaining = remaining.saturating_sub(charge);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Clock, Pacer};
    use std::{num::NonZeroU64, time::Duration};

    #[derive(Default)]
    struct FakeClock {
        elapsed: Duration,
        sleeps: usize,
    }

    impl Clock for FakeClock {
        fn now(&self) -> Duration {
            self.elapsed
        }
        fn sleep(&mut self, duration: Duration) {
            self.elapsed = self.elapsed.saturating_add(duration);
            self.sleeps = self.sleeps.saturating_add(1);
        }
    }

    #[test]
    fn steady_state_and_every_one_second_window() {
        let rate = 256 << 20;
        let chunk = 1 << 20;
        let mut pacer = Pacer::new(FakeClock::default(), NonZeroU64::new(rate), chunk);
        let mut reads = Vec::new();
        for index in 0..2000 {
            // Include bootstrap sizes, partial chunks and idle periods that refill the bucket.
            let bytes = if index % 13 == 0 { 512 } else { chunk };
            if index % 71 == 0 {
                pacer.clock.elapsed += Duration::from_millis(30);
            }
            pacer.acquire(bytes);
            reads.push((pacer.clock.now(), bytes));
        }
        for &(start, _) in &reads {
            let bytes: usize = reads
                .iter()
                .filter(|(time, _)| *time >= start && *time < start + Duration::from_secs(1))
                .map(|(_, bytes)| bytes)
                .sum();
            assert!(bytes as u64 <= rate, "window at {start:?}: {bytes}");
        }
        let mut steady = Pacer::new(FakeClock::default(), NonZeroU64::new(rate), chunk);
        for _ in 0..255 {
            steady.acquire(chunk);
        }
        assert!(steady.clock.now() >= Duration::from_secs(1));
        assert!(steady.clock.now() < Duration::from_millis(1001));
    }

    #[test]
    fn idle_burst_is_at_most_one_chunk() {
        let mut pacer = Pacer::new(FakeClock::default(), NonZeroU64::new(1024), 512);
        pacer.clock.elapsed = Duration::from_secs(10);
        pacer.acquire(512);
        assert_eq!(pacer.clock.sleeps, 0);
        pacer.acquire(512);
        assert_eq!(pacer.clock.now(), Duration::from_secs(11));
    }

    #[test]
    fn uncapped_never_sleeps() {
        let mut pacer = Pacer::new(FakeClock::default(), None, 512);
        pacer.acquire(usize::MAX);
        assert_eq!(pacer.clock.sleeps, 0);
        assert_eq!(pacer.clock.now(), Duration::ZERO);
    }

    #[test]
    fn oversized_requests_and_rates_below_one_chunk_progress() {
        let mut pacer = Pacer::new(FakeClock::default(), NonZeroU64::new(128), 512);
        pacer.acquire(2048);
        assert_eq!(pacer.clock.now(), Duration::from_secs(16));
        assert_eq!(pacer.clock.sleeps, 4);
    }
}
