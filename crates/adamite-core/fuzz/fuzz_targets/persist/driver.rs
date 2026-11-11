use adamite_core::{
    scan::ScanOutput,
    snapshot::{NodeId, Snapshot},
};
use std::hint::black_box;

pub(super) fn exercise(bytes: &[u8]) -> bool {
    let accepted = check(bytes);
    if bytes.len() >= 64 {
        let mut repaired = bytes.to_vec();
        repaired[32..36].fill(0);
        let crc = crc32fast::hash(&repaired);
        repaired[32..36].copy_from_slice(&crc.to_le_bytes());
        check(&repaired);
    }
    accepted
}

fn check(bytes: &[u8]) -> bool {
    let Ok(output) = ScanOutput::from_bytes(bytes) else {
        return false;
    };
    visit(&output.snapshot);
    assert_eq!(
        output.to_bytes().expect("accepted snapshot encodes"),
        bytes,
        "accepted encoding must be canonical"
    );
    true
}

fn visit(snapshot: &Snapshot) {
    assert!(!snapshot.is_empty(), "accepted snapshot has a root");
    black_box(snapshot.volume().geometry().fields());
    black_box(snapshot.volume().context());
    for index in 0..snapshot.len() {
        let id = NodeId::from_raw(u32::try_from(index).expect("node id fits v1"));
        let node = snapshot.node(id).expect("node in accepted arena");
        black_box((
            node.id(),
            node.size(),
            node.alloc(),
            node.total_size(),
            node.total_alloc(),
            node.mtime(),
            node.parent(),
            node.child_start(),
            node.child_len(),
            node.file_count(),
            node.name_off(),
            node.name_len(),
            node.flags(),
            node.ads_size(),
            node.file_ref(),
            node.attrs(),
            node.nlink(),
            node.reparse_tag(),
        ));
        black_box(snapshot.children(id).expect("accepted child range"));
        black_box(snapshot.name(id).expect("accepted node name"));
        if index < 64 {
            assert!(snapshot.path(id).expect("accepted path").is_some());
        }
    }
    let mut pending = vec![(NodeId::ROOT, 0usize)];
    let mut deepest = (NodeId::ROOT, 0);
    let mut visited = 0;
    while let Some((id, depth)) = pending.pop() {
        visited += 1;
        assert!(visited <= snapshot.len(), "accepted tree has no cycle");
        if depth > deepest.1 {
            deepest = (id, depth);
        }
        for &child in snapshot.children(id).expect("accepted traversal range") {
            pending.push((child, depth + 1));
        }
    }
    assert_eq!(visited, snapshot.len(), "accepted tree is connected");
    assert!(snapshot.path(deepest.0).expect("deepest path").is_some());
    for &id in snapshot.top_files() {
        black_box(snapshot.node(id).expect("accepted top file"));
        black_box(snapshot.name(id).expect("accepted top file name"));
    }
    for stat in snapshot.ext_stats() {
        black_box((
            stat.extension(),
            stat.count(),
            stat.total_size(),
            stat.total_alloc(),
        ));
    }
}
