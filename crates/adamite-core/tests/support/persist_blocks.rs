use adamite_core::scan::ScanOutput;

/// Exercises the same two-pass decoder with independently fragmented input blocks.
pub fn decode_blocks(
    bytes: &[u8],
    block_size: usize,
) -> Result<ScanOutput, adamite_core::snapshot::PersistError> {
    use adamite_core::snapshot::SnapshotDecoder;
    let mut first = SliceBlocks::new(bytes, block_size);
    let decoder = SnapshotDecoder::verify(bytes.len(), |target| first.read(target))?;
    let mut second = SliceBlocks::new(bytes, block_size);
    let (snapshot, stats) = decoder.decode(|target| second.read(target))?;
    Ok(ScanOutput { snapshot, stats })
}

pub fn decode_checked(bytes: &[u8]) -> Result<ScanOutput, adamite_core::snapshot::PersistError> {
    let expected = ScanOutput::from_bytes(bytes);
    for size in [1, 7, 4096, bytes.len().max(1)] {
        let actual = decode_blocks(bytes, size);
        match (&expected, &actual) {
            (Ok(expected), Ok(actual)) => assert_eq!(expected, actual, "block size {size}"),
            (Err(expected), Err(actual)) => assert_eq!(
                format!("{expected:?}"),
                format!("{actual:?}"),
                "block size {size}"
            ),
            _ => panic!("block size {size}: expected {expected:?}, got {actual:?}"),
        }
    }
    expected
}

struct SliceBlocks<'a> {
    blocks: std::slice::Chunks<'a, u8>,
    current: &'a [u8],
}

impl<'a> SliceBlocks<'a> {
    fn new(bytes: &'a [u8], size: usize) -> Self {
        Self {
            blocks: bytes.chunks(size),
            current: &[],
        }
    }

    fn read(&mut self, mut target: &mut [u8]) -> Result<(), adamite_core::snapshot::PersistError> {
        while !target.is_empty() {
            if self.current.is_empty() {
                self.current =
                    self.blocks
                        .next()
                        .ok_or(adamite_core::snapshot::PersistError::Truncated {
                            needed: target.len(),
                            available: 0,
                        })?;
            }
            let len = target.len().min(self.current.len());
            let (head, tail) = target.split_at_mut(len);
            head.copy_from_slice(&self.current[..len]);
            target = tail;
            self.current = &self.current[len..];
        }
        Ok(())
    }
}
