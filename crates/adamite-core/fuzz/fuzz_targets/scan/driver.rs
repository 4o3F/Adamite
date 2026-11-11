//! Public protocol checks, including a valid-feed-only reference scanner.

use adamite_core::{
    scan::{FeedError, ReadRequest, RequestId, ScanConfig, ScanStep, Scanner},
    snapshot::{NodeFlags, NodeId, Snapshot},
};

#[derive(Debug)]
struct Slot {
    request: ReadRequest,
    accepted: bool,
}

fn refresh(scanner: &mut Scanner, slots: &mut Vec<Slot>) {
    slots.retain(|slot| {
        if !slot.accepted {
            return true;
        }
        match scanner.feed(slot.request.id, &[]) {
            Err(FeedError::DuplicateResponse { id }) => {
                assert_eq!(id, slot.request.id);
                true
            }
            Err(FeedError::UnknownRequest { .. } | FeedError::Inactive) => false,
            other => panic!("accepted-slot probe changed behavior: {other:?}"),
        }
    });
}

fn reject_unknown(scanner: &mut Scanner) {
    let unknown = RequestId::from_raw(u64::MAX);
    assert!(
        matches!(scanner.feed(unknown, &[]), Err(FeedError::UnknownRequest { id }) if id == unknown)
    );
}

fn permutation(mut indexes: Vec<usize>, mut rank: usize) -> Vec<usize> {
    let mut result = Vec::with_capacity(indexes.len());
    while !indexes.is_empty() {
        let factorial: usize = (1..indexes.len()).product();
        let position = (rank / factorial) % indexes.len();
        rank %= factorial;
        result.push(indexes.remove(position));
    }
    result
}

pub(super) fn scan(image: &[u8], config: ScanConfig, events: &[u8; 8], records: usize) {
    let cap = config.max_outstanding;
    let chunk = config.chunk_size;
    let mut scanner = Scanner::new(config.clone()).expect("bounded transport config");
    let mut reference = Scanner::new(config).expect("bounded transport config");
    let mut slots = Vec::<Slot>::new();
    let limit = 4096 + 64 * records + 16 * image.len().div_ceil(512);
    for calls in 0..limit {
        let step = match (scanner.step(), reference.step()) {
            (Ok(actual), Ok(expected)) => {
                assert_eq!(actual, expected, "invalid feeds changed scan state");
                actual
            }
            (Err(actual), Err(expected)) => {
                assert_eq!(
                    std::mem::discriminant(&actual),
                    std::mem::discriminant(&expected)
                );
                assert_eq!(
                    actual.to_string(),
                    expected.to_string(),
                    "invalid feeds changed the terminal error"
                );
                return;
            }
            other => panic!("invalid feeds changed scan state: {other:?}"),
        };
        refresh(&mut scanner, &mut slots);
        if let ScanStep::Need(request) = &step {
            assert!(
                request.len > 0 && request.len <= chunk,
                "invalid request {request:?}"
            );
            assert!(
                request
                    .offset
                    .checked_add(u64::try_from(request.len).expect("bounded length"))
                    .is_some(),
                "request overflow"
            );
            slots.push(Slot {
                request: *request,
                accepted: false,
            });
        }
        assert!(slots.len() <= cap, "outstanding cap exceeded");
        let outstanding_bytes: usize = slots.iter().map(|slot| slot.request.len).sum();
        assert!(
            outstanding_bytes <= cap * chunk,
            "outstanding byte bound exceeded"
        );
        let event = events[calls % events.len()];
        match step {
            ScanStep::Need(request) => {
                let end = request.offset + u64::try_from(request.len).expect("bounded length");
                if end > u64::try_from(image.len()).expect("bounded image") {
                    return;
                }
                if event & 0x20 != 0 {
                    reject_unknown(&mut scanner);
                }
            }
            ScanStep::Progress(progress) => {
                assert_eq!(progress.outstanding, slots.len());
                if progress.waiting_for_input {
                    let pending: Vec<_> = slots
                        .iter()
                        .enumerate()
                        .filter_map(|(index, slot)| (!slot.accepted).then_some(index))
                        .collect();
                    assert!(!pending.is_empty(), "waiting without an unfilled request");
                    for index in permutation(pending, usize::from(event & 31)) {
                        let slot = &mut slots[index];
                        let request = slot.request;
                        if event & 0x40 != 0 {
                            assert!(matches!(scanner.feed(request.id, &[]),
                                Err(FeedError::LengthMismatch { id, expected, actual: 0 })
                                if id == request.id && expected == request.len));
                        }
                        let start = usize::try_from(request.offset).expect("in-image offset");
                        let bytes = &image[start..start + request.len];
                        scanner.feed(request.id, bytes).expect("correct response");
                        reference
                            .feed(request.id, bytes)
                            .expect("reference response");
                        slot.accepted = true;
                        if event & 0x80 != 0 {
                            assert!(matches!(scanner.feed(request.id, bytes),
                                Err(FeedError::DuplicateResponse { id }) if id == request.id));
                        }
                    }
                }
            }
            ScanStep::Done(output) => {
                assert!(slots.is_empty(), "Done retained transport slots");
                aggregation(&output.snapshot);
                return;
            }
            _ => panic!("new scan protocol variant needs harness support"),
        }
    }
    panic!(
        "scan exceeded {limit} driver calls for {records} records and {} image bytes",
        image.len()
    );
}

fn aggregation(snapshot: &Snapshot) {
    assert!(!snapshot.is_empty());
    let mut seen = vec![false; snapshot.len()];
    let mut csr_cursor = 0;
    for index in 0..snapshot.len() {
        let id = NodeId::from_raw(u32::try_from(index).expect("bounded node arena"));
        let node = snapshot.node(id).expect("node in arena");
        let children = snapshot.children(id).expect("CSR range");
        assert_eq!(
            usize::try_from(node.child_start()).expect("CSR offset"),
            csr_cursor
        );
        assert_eq!(
            usize::try_from(node.child_len()).expect("CSR length"),
            children.len()
        );
        csr_cursor += children.len();
        let mut size = node.size();
        let mut alloc = node.alloc();
        let mut files = u32::from(
            !node.is_dir()
                && !node.flags().contains(NodeFlags::SYNTHETIC)
                && !node.flags().contains(NodeFlags::HARDLINK_DUP),
        );
        for &child in children {
            assert_ne!(child, NodeId::ROOT);
            let child_index = usize::try_from(child.raw()).expect("child index");
            assert!(!seen[child_index], "repeated CSR child");
            seen[child_index] = true;
            let child = snapshot.node(child).expect("child in arena");
            assert_eq!(child.parent(), id);
            if !child.flags().contains(NodeFlags::HARDLINK_DUP) {
                size = size
                    .checked_add(child.total_size())
                    .expect("subtree logical overflow");
                alloc = alloc
                    .checked_add(child.total_alloc())
                    .expect("subtree allocation overflow");
                files = files
                    .checked_add(child.file_count())
                    .expect("subtree count overflow");
            }
        }
        assert_eq!(
            (node.total_size(), node.total_alloc(), node.file_count()),
            (size, alloc, files)
        );
    }
    assert_eq!(csr_cursor, snapshot.len() - 1);
    assert!(
        seen[1..].iter().all(|&present| present),
        "missing CSR child"
    );
    assert!(snapshot.top_files().len() <= 100);
}
