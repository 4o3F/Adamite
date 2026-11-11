use std::fmt;

/// Formats byte counts as binary units with one decimal place, or as a raw integer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ByteSize {
    /// The number of bytes to display.
    pub value: u128,
    /// Prints the integer alone when true; otherwise includes a binary unit.
    pub raw_bytes: bool,
}

impl fmt::Display for ByteSize {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.raw_bytes {
            return write!(f, "{}", self.value);
        }
        let mut divisor = 1u128;
        let mut unit = "B";
        for candidate in ["KiB", "MiB", "GiB", "TiB", "PiB", "EiB"] {
            let Some(next) = divisor.checked_mul(1024) else {
                break;
            };
            if self.value < next {
                break;
            }
            divisor = next;
            unit = candidate;
        }
        if divisor == 1 {
            return write!(f, "{} B", self.value);
        }
        let fractional = self
            .value
            .checked_rem(divisor)
            .and_then(|value| value.checked_mul(10))
            .and_then(|value| value.checked_add(divisor / 2))
            .and_then(|value| value.checked_div(divisor))
            .ok_or(fmt::Error)?;
        let tenths = self
            .value
            .checked_div(divisor)
            .and_then(|value| value.checked_mul(10))
            .and_then(|value| value.checked_add(fractional))
            .ok_or(fmt::Error)?;
        write!(f, "{}.{:01} {unit}", tenths / 10, tenths % 10)
    }
}

#[cfg(test)]
mod tests {
    use super::ByteSize;

    #[test]
    fn binary_units_round_and_raw_counts_preserve_the_full_range() {
        for (value, expected) in [
            (0, "0 B"),
            (1023, "1023 B"),
            (1024, "1.0 KiB"),
            (1536, "1.5 KiB"),
            (1048575, "1024.0 KiB"),
            (1048576, "1.0 MiB"),
            (u128::from(u64::MAX), "16.0 EiB"),
            (u128::MAX, "295147905179352825856.0 EiB"),
        ] {
            assert_eq!(
                ByteSize {
                    value,
                    raw_bytes: false
                }
                .to_string(),
                expected
            );
            assert_eq!(
                ByteSize {
                    value,
                    raw_bytes: true
                }
                .to_string(),
                value.to_string()
            );
        }
    }
}
