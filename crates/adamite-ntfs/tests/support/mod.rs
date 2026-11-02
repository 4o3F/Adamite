#![allow(dead_code)]

pub mod builders;
pub mod fixture;
pub mod runlist_encoder;

pub fn proptest_config(path: &'static str) -> proptest::test_runner::Config {
    proptest::test_runner::Config {
        failure_persistence: Some(Box::new(
            proptest::test_runner::FileFailurePersistence::Direct(path),
        )),
        ..Default::default()
    }
}
