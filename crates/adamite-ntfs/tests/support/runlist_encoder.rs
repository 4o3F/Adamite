#![allow(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

//! Independent semantic run encoder using wide integer arithmetic.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunModel {
    pub len: u64,
    pub lcn: Option<u64>,
}

pub struct RunlistEncoder {
    pub runs: Vec<RunModel>,
}

impl RunlistEncoder {
    pub fn encode(&self, minimum_width: usize) -> Vec<u8> {
        assert!(minimum_width <= 8);
        let mut bytes = Vec::new();
        let mut previous = 0i128;
        for run in &self.runs {
            let length_width = (1..=8)
                .find(|width| u128::from(run.len) < 2u128.pow((width * 8) as u32))
                .unwrap()
                .max(minimum_width);
            let (offset_width, delta) = if let Some(lcn) = run.lcn {
                let delta = i128::from(lcn) - previous;
                let width = (1..=8)
                    .find(|width| {
                        let limit = 2i128.pow((width * 8 - 1) as u32);
                        (-limit..limit).contains(&delta)
                    })
                    .unwrap()
                    .max(minimum_width);
                previous = i128::from(lcn);
                (width, delta)
            } else {
                (0, 0)
            };
            bytes.push(((offset_width << 4) | length_width) as u8);
            bytes.extend_from_slice(&run.len.to_le_bytes()[..length_width]);
            bytes.extend_from_slice(&delta.to_le_bytes()[..offset_width]);
        }
        bytes.push(0);
        bytes
    }
}
