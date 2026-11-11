#![no_main]

#[path = "persist/driver.rs"]
mod driver;

use libfuzzer_sys::fuzz_target;

fuzz_target!(|bytes: &[u8]| {
    driver::exercise(bytes);
});
