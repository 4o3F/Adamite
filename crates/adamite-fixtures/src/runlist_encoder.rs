//! Independent semantic run encoder using wide integer arithmetic.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// One semantic run for independent mapping-pair encoding.
pub struct RunModel {
    /// Run length in clusters.
    pub len: u64,
    /// Physical cluster, absent for a sparse run.
    pub lcn: Option<u64>,
}

/// Independent mapping-pair encoder with per-segment LCN delta state.
#[derive(Debug, Clone)]
pub struct RunlistEncoder {
    /// Semantic runs to encode in their supplied order.
    pub runs: Vec<RunModel>,
}

impl RunlistEncoder {
    /// Encodes runs with at least the requested field width.
    ///
    /// # Panics
    ///
    /// Panics if the configured lengths, offsets or values cannot fit the encoded format.
    #[must_use]
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
