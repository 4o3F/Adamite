#![no_main]

#[path = "scan/driver.rs"]
mod driver;
#[path = "scan/input.rs"]
mod input;
#[path = "scan/volume.rs"]
mod volume;

use libfuzzer_sys::fuzz_target;

fuzz_target!(|bytes: &[u8]| {
    let Ok(input) = input::Input::parse(bytes) else {
        return;
    };
    let image = input.image();
    let Some(records) = input::preflight(&image) else {
        return;
    };
    driver::scan(&image, input.config(), &input.events, records);
});
