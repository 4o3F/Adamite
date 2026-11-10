//! Small independent NTFS layouts with cross-extent records and an early patch.

use adamite_fixtures::{
    builders::{
        AttrBody, AttrBuilder, AttrListBuilder, AttrListEntryModel, BootSectorBuilder,
        FileNameBuilder, NonResidentModel, RecordBuilder,
    },
    runlist_encoder::{RunModel, RunlistEncoder},
};

const RECORD_SIZE: usize = 1024;
const MFT_SIZE: i64 = 24 * 1024;
const SEQUENCE: u64 = 7 << 48;

#[derive(Debug, Clone, Copy)]
pub(super) enum Layout {
    Contiguous,
    Fragmented,
}

fn name(parent: u32, text: &str, namespace: u8, id: u16) -> Vec<u8> {
    let mut attribute = AttrBuilder::resident(
        0x30,
        FileNameBuilder {
            parent: SEQUENCE | u64::from(parent),
            name: text.encode_utf16().collect(),
            namespace,
            ..FileNameBuilder::default()
        }
        .build(),
    );
    attribute.id = id;
    attribute.build()
}

fn segment(id: u16, lowest: i64, run: RunModel) -> Vec<u8> {
    let mut data = AttrBuilder::non_resident(
        0x80,
        NonResidentModel {
            lowest,
            highest: lowest + i64::try_from(run.len).expect("bounded clusters") - 1,
            sizes: [MFT_SIZE, MFT_SIZE, MFT_SIZE, 0],
            mapping_pairs: RunlistEncoder { runs: vec![run] }.encode(0),
            ..NonResidentModel::default()
        },
    );
    data.id = id;
    data.build()
}

fn put(image: &mut [u8], runs: &[RunModel], record: RecordBuilder) {
    let bytes = record.build();
    let start = usize::try_from(record.record_number).expect("bounded ordinal") * RECORD_SIZE;
    let mut virtual_start = 0;
    for run in runs {
        let len = usize::try_from(run.len).expect("bounded clusters") * 512;
        let first = start.max(virtual_start);
        let end = (start + RECORD_SIZE).min(virtual_start + len);
        if first < end {
            let physical =
                usize::try_from(run.lcn.expect("mapped run")).expect("bounded LCN") * 512 + first
                    - virtual_start;
            image[physical..physical + end - first]
                .copy_from_slice(&bytes[first - start..end - start]);
        }
        virtual_start += len;
    }
}

pub(super) fn build(layout: Layout, options: u8) -> Vec<u8> {
    let mut image = vec![0; 128 * 512];
    image[..512].copy_from_slice(
        &BootSectorBuilder {
            cluster_encoding: 1,
            sectors: 128,
            mirror: 124,
            ..BootSectorBuilder::default()
        }
        .build(),
    );
    let runs = match layout {
        Layout::Contiguous => vec![RunModel {
            len: 48,
            lcn: Some(4),
        }],
        Layout::Fragmented => vec![
            RunModel {
                len: 13,
                lcn: Some(4),
            },
            RunModel {
                len: 12,
                lcn: Some(48),
            },
            RunModel {
                len: 23,
                lcn: Some(96),
            },
        ],
    };
    let mut attributes = vec![segment(0, 0, runs[0]), name(5, "$MFT", 1, 4)];
    if matches!(layout, Layout::Fragmented) {
        let list = AttrListBuilder {
            entries: [(10, 2, 25), (1, 1, 13)]
                .map(|(record, id, lowest)| AttrListEntryModel {
                    type_code: 0x80,
                    lowest,
                    segment: SEQUENCE | record,
                    id,
                    name: vec![],
                })
                .to_vec(),
            zero_tail: 0,
        };
        let mut attribute = AttrBuilder::resident(0x20, list.build());
        attribute.id = 3;
        attributes.push(attribute.build());
        for (number, id, lowest, run) in [(1, 1, 13, runs[1]), (10, 2, 25, runs[2])] {
            put(
                &mut image,
                &runs,
                RecordBuilder {
                    record_number: number,
                    base: SEQUENCE,
                    attributes: vec![segment(id, lowest, run)],
                    ..RecordBuilder::default()
                },
            );
        }
    }
    put(
        &mut image,
        &runs,
        RecordBuilder {
            record_number: 0,
            attributes,
            ..RecordBuilder::default()
        },
    );
    for (number, parent, text) in [(5, 5, "."), (16, 5, "dir"), (17, 16, "deep")] {
        put(
            &mut image,
            &runs,
            RecordBuilder {
                record_number: number,
                flags: 3,
                attributes: vec![name(parent, text, 1, 0)],
                ..RecordBuilder::default()
            },
        );
    }
    for number in 18..=21 {
        let parent = if options & 1 == 0 { 5 } else { 17 };
        let text = if options & 2 == 0 {
            format!("file{number}.txt")
        } else {
            format!("文件🦀{number}.gz")
        };
        let mut attributes = vec![name(parent, &text, 1, 0), name(parent, "FILE~1.TXT", 2, 1)];
        let hard_links = if options & 4 == 0 {
            1
        } else {
            attributes.push(name(parent, &format!("alias{number}.bin"), 0, 2));
            attributes.push(name(16, &format!("cross{number}.txt"), 0, 3));
            3
        };
        let mut data = if number % 2 == 0 {
            AttrBuilder::resident(0x80, vec![options; usize::from(options % 32)])
        } else {
            AttrBuilder::non_resident(
                0x80,
                NonResidentModel {
                    sizes: [512, i64::from(options), i64::from(options), 0],
                    mapping_pairs: RunlistEncoder {
                        runs: vec![RunModel {
                            len: 1,
                            lcn: Some(80 + u64::from(number - 18)),
                        }],
                    }
                    .encode(0),
                    ..NonResidentModel::default()
                },
            )
        };
        data.id = 4;
        // Some seeds have sparse storage; allocation remains independently encoded.
        if options & 8 != 0 && number % 2 != 0 {
            data.flags = 0x8000;
            if let AttrBody::NonResident(model) = &mut data.body {
                model.sizes[3] = 512;
            }
        }
        attributes.push(data.build());
        let mut ads = AttrBuilder::resident(0x80, vec![1, 2, 3]);
        ads.name = "notes".encode_utf16().collect();
        ads.id = 5;
        if number == 18 {
            put(
                &mut image,
                &runs,
                RecordBuilder {
                    record_number: 2,
                    base: SEQUENCE | u64::from(number),
                    attributes: vec![ads.build()],
                    ..RecordBuilder::default()
                },
            );
        } else {
            attributes.push(ads.build());
        }
        put(
            &mut image,
            &runs,
            RecordBuilder {
                record_number: number,
                attributes,
                hard_links,
                ..RecordBuilder::default()
            },
        );
    }
    image
}
