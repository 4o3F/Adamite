use std::fmt;

pub(crate) struct Size {
    pub(crate) value: u64,
    pub(crate) bytes: bool,
}

impl fmt::Display for Size {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.bytes {
            return write!(f, "{}", self.value);
        }
        let mut divisor = 1u64;
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
        let tenths = u128::from(self.value)
            .checked_mul(10)
            .and_then(|value| value.checked_add(u128::from(divisor / 2)))
            .and_then(|value| value.checked_div(u128::from(divisor)))
            .ok_or(fmt::Error)?;
        write!(f, "{}.{:01} {unit}", tenths / 10, tenths % 10)
    }
}
