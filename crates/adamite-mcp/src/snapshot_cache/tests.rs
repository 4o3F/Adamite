use super::{LOAD_PERMIT, SnapshotCache};
use crate::{McpError, tests::Case};
use std::{
    error::Error,
    fs,
    sync::{Arc, Weak},
    thread,
};

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

#[test]
fn concurrent_same_path_requests_share_exactly_one_uncached_output() -> TestResult {
    let case = Case::new()?;
    let path = case.snapshot("C.adm")?;
    let canonical = fs::canonicalize(&path)?;
    let cache = SnapshotCache::new(0);
    thread::scope(|scope| -> TestResult {
        // Hold the production permit until every caller has joined the same pending load.
        let permit = LOAD_PERMIT.lock().map_err(McpError::poisoned)?;
        let workers: Vec<_> = (0..8).map(|_| scope.spawn(|| cache.load(&path))).collect();
        let ready = Case::wait_until(|| {
            cache.state.lock().is_ok_and(|state| {
                state
                    .loading
                    .get(&canonical)
                    .is_some_and(|pending| Arc::strong_count(pending) == 9)
            })
        });
        drop(permit);
        ready?;
        let outputs: Vec<_> = workers
            .into_iter()
            .map(|worker| -> TestResult<_> {
                Ok(worker.join().map_err(|_| "load worker panicked")??)
            })
            .collect::<Result<_, _>>()?;
        let first = outputs.first().ok_or("missing worker output")?;
        assert!(outputs.iter().all(|output| Arc::ptr_eq(first, output)));
        let previous = Arc::downgrade(first);
        drop(outputs);
        assert!(
            previous.upgrade().is_none(),
            "zero capacity must retain no output"
        );
        let next = cache.load(&path)?;
        assert_eq!(Weak::strong_count(&previous), 0);
        assert!(!next.snapshot.is_empty());
        Ok(())
    })
}
