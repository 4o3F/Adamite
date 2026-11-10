//! Generic parent linking and subtree aggregation.

mod aggregate;
mod finalize;

pub(crate) use aggregate::Aggregator;
pub(crate) use finalize::Finalizer;
pub(crate) mod link;
