import {
  blocks,
  compactManifest,
  type Entry,
  extents,
  fileAttributes,
  geometry,
  integer,
  json,
  type Manifest,
  reparseMetadata,
  streams,
  units,
} from "./manifest.ts";
import { checkFragmentation } from "./populate/frag-512.ts";

function assert(condition: boolean, message: string) {
  if (!condition) throw new Error(message);
}
const data = `Dumping attribute $DATA (0x80) from mft record 0 (0x0)
 Resident: No
 Attribute flags: 0x0000
 Lowest VCN 0 (0x0)
 Data size: 18446744073709551615 (0xffffffffffffffff)
 Runlist: VCN LCN Length
 0x0 0x4 0x8
 0x8 0x20 0x2
`;

Deno.test("u64 values serialize as exact decimal strings", () => {
  const parsed = JSON.parse(json({ value: 18446744073709551615n }));
  assert(parsed.value === "18446744073709551615", "u64 precision was lost");
  assert(integer(data, "Data size") === parsed.value, "tool number was rounded");
  assert(JSON.stringify(units("😀")) === "[55357,56832]", "surrogate pair was changed");
});

Deno.test("manifest schema and independent tool output", () => {
  const output =
    "Sector Size: 512\nCluster Size: 4096\nMFT Record Size: 1024\nIndex Block Size: 4096\nVolume Size in Clusters: 4095\nLCN of Data Attribute for FILE_MFT: 4\nLCN of Data Attribute for File_MFTMirr: 2047\n";
  const manifest: Manifest = {
    schema_version: 2,
    image: "sample.img.zst",
    image_len: "16777216",
    generator: { mkntfs: "test" },
    geometry: geometry(output),
    mft: { extents: extents(data), data_size: "8192", has_attribute_list: false },
    entries: [{
      path: "/",
      record: "5",
      seq: 5,
      kind: "directory",
      nlink: 1,
      attributes: 16,
      mtime_filetime: "132223104000000000",
      names: [{ parent_record: "5", parent_seq: 5, name: [46], namespace: 3 }],
      streams: streams(data),
      reparse_tag: null,
      has_attribute_list: false,
    }],
    deleted: [{ path: "/deleted.txt", record: "42" }],
    fill: [],
  };
  const parsed = JSON.parse(json(manifest));
  assert(
    Object.keys(parsed).sort().join() ===
      "deleted,entries,fill,generator,geometry,image,image_len,mft,schema_version",
    "top-level schema changed",
  );
  assert(
    parsed.geometry.total_clusters === "4095" && parsed.entries[0].names[0].parent_seq === 5,
    "nested schema changed",
  );
  assert(parsed.entries[0].streams[0].size === "18446744073709551615", "stream size was rounded");
  assert(
    blocks(data, 0x80).length === 1 && parsed.mft.extents.length === 2,
    "runlist was not extracted",
  );
  checkFragmentation(data, "Dumping attribute $ATTRIBUTE_LIST (0x20)\nStarting VCN: 600 (0x258)\n");
  let rejected = false;
  try {
    checkFragmentation(data.replace(" 0x8 0x20 0x2\n", ""), "");
  } catch {
    rejected = true;
  }
  assert(rejected, "fragmentation postconditions were not enforced");
});

Deno.test("filler summaries and compact serialization are deterministic and preserve names", () => {
  const parent: Entry = {
    path: "/fill",
    record: "64",
    seq: 7,
    kind: "directory",
    nlink: 1,
    attributes: 0,
    mtime_filetime: "132223104000000000",
    names: [],
    streams: [],
    reparse_tag: null,
    has_attribute_list: false,
  };
  const children = [67, 69].map((record, index): Entry => ({
    ...parent,
    path: `/fill/f${index}`,
    record: String(record),
    kind: "file",
    names: [{ parent_record: "64", parent_seq: 7, name: [102, 48 + index], namespace: 0 }],
  }));
  const input = {
    schema_version: 1,
    image: "sample.img.zst",
    image_len: "16777216",
    generator: {},
    geometry: geometry(
      "Sector Size: 512\nCluster Size: 4096\nMFT Record Size: 1024\nIndex Block Size: 4096\nVolume Size in Clusters: 4095\nLCN of Data Attribute for FILE_MFT: 4\nLCN of Data Attribute for File_MFTMirr: 2047\n",
    ),
    mft: { extents: extents(data), data_size: "8192", has_attribute_list: false },
    entries: [parent, ...children],
    deleted: [],
  };
  const compact = compactManifest(input);
  assert(compact.schema_version === 2 && compact.entries.length === 1, "filler entries remain");
  assert(
    JSON.stringify(compact.fill) === JSON.stringify([{
      dir: "/fill",
      count: 2,
      name_prefix: "f",
      name_len: 2,
      record_min: "67",
      record_max: "69",
      parent_record: "64",
      parent_seq: 7,
    }]),
    "filler summary differs",
  );
  assert(json(compact) === json(compactManifest(compact)), "conversion is not idempotent");
  assert(json({ name: [55357, 56832] }).includes("[55357,56832]"), "numeric array was expanded");
  assert(input.entries.length === 3, "conversion mutated its input");
});

// Captured with ntfsinfo 2022.10.3 -v -F /reparse.bin after mkntfs of a 16 MiB
// image and ntfscp of an empty file, without mounting. Adapted SI flags/mtime
// and added REPARSE_POINT; FILE_NAME retains its captured, stale metadata.
const reparseSample = `Dumping Inode 64 (0x40)
Upd. Seq. Array Off.:\t 48 (0x30)
Upd. Seq. Array Count:\t 3 (0x3)
Upd. Seq. Number:\t 3 (0x3)
LogFile Seq. Number:\t 0x0
MFT Record Seq. Numb.:\t 1 (0x1)
Number of Hard Links:\t 1 (0x1)
Attribute Offset:\t 56 (0x38)
MFT Record Flags:\t IN_USE
Bytes Used:\t\t 456 (0x1c8) bytes
Bytes Allocated:\t 1024 (0x400) bytes
Next Attribute Instance: 5 (0x5)
MFT Padding:\t00 00
Dumping attribute $STANDARD_INFORMATION (0x10) from mft record 64 (0x40)
\tAttribute length:\t 72 (0x48)
\tResident: \t\t Yes
\tName length:\t\t 0 (0x0)
\tName offset:\t\t 0 (0x0)
\tAttribute flags:\t 0x0000
\tAttribute instance:\t 0 (0x0)
\tData size:\t\t 48 (0x30)
\tData offset:\t\t 24 (0x18)
\tResident flags:\t\t 0x00
\tReservedR:\t\t 0 (0x0)
\tFile Creation Time:\t Wed Jan  1 00:00:00 2020 UTC
\tFile Altered Time:\t Wed Jan  1 00:00:01 2020 UTC
\tMFT Changed Time:\t Wed Jan  1 00:00:01 2020 UTC
\tLast Accessed Time:\t Wed Jan  1 00:00:00 2020 UTC
\tFile attributes:\t ARCHIVE REPARSE_POINT (0x00000420)
Dumping attribute $FILE_NAME (0x30) from mft record 64 (0x40)
\tAttribute length:\t 112 (0x70)
\tResident: \t\t Yes
\tName length:\t\t 0 (0x0)
\tName offset:\t\t 0 (0x0)
\tAttribute flags:\t 0x0000
\tAttribute instance:\t 3 (0x3)
\tData size:\t\t 88 (0x58)
\tData offset:\t\t 24 (0x18)
\tResident flags:\t\t 0x01
\tReservedR:\t\t 0 (0x0)
\tParent directory:\t 5 (0x5)
\tFile Creation Time:\t Mon Nov  2 04:42:31 2026 UTC
\tFile Altered Time:\t Mon Nov  2 04:42:31 2026 UTC
\tMFT Changed Time:\t Mon Nov  2 04:42:31 2026 UTC
\tLast Accessed Time:\t Mon Nov  2 04:42:31 2026 UTC
\tAllocated Size:\t\t 0 (0x0)
\tData Size:\t\t 0 (0x0)
\tFilename Length:\t 11 (0xb)
\tFile attributes:\t ARCHIVE (0x00000020)
\tNamespace:\t\t POSIX
\tFilename:\t\t 'reparse.bin'
Dumping attribute $DATA (0x80) from mft record 64 (0x40)
\tAttribute length:\t 24 (0x18)
\tResident: \t\t Yes
\tName length:\t\t 0 (0x0)
\tName offset:\t\t 0 (0x0)
\tAttribute flags:\t 0x0000
\tAttribute instance:\t 2 (0x2)
\tData size:\t\t 0 (0x0)
\tData offset:\t\t 24 (0x18)
\tResident flags:\t\t 0x00
\tReservedR:\t\t 0 (0x0)
Dumping attribute $REPARSE_POINT (0xc0) from mft record 64 (0x40)
\tAttribute length:\t 80 (0x50)
\tResident: \t\t Yes
\tName length:\t\t 0 (0x0)
\tName offset:\t\t 0 (0x0)
\tAttribute flags:\t 0x0000
\tAttribute instance:\t 4 (0x4)
\tData size:\t\t 52 (0x34)
\tData offset:\t\t 24 (0x18)
\tResident flags:\t\t 0x00
\tReservedR:\t\t 0 (0x0)
\tReparse tag:\t\t 0xa000000c (SYMLINK)
\tData length:\t\t 44 (0x2c)
\tData:\t\t\t 0x000010001000100001000000740069006e0079002e00740078007400740069006e00...
End of inode reached
Total runs: 0 (fragments: 0)
`;

Deno.test("offline reparse metadata overrides the collected regular file metadata", () => {
  const metadata = reparseMetadata(reparseSample);
  assert(metadata.reparse_tag === 0xa000000c, "symlink reparse tag was lost");
  assert(metadata.attributes === 0x420, "stale FILE_NAME flags were used");
  assert(metadata.kind === "symlink", "regular placeholder kind was kept");
  assert(
    metadata.mtime_filetime === "132223104010000000",
    "offline SI mtime must replace the collected 132223104000000000",
  );
  const epoch = reparseSample.replace(
    "Wed Jan  1 00:00:01 2020 UTC",
    "Mon Jan  1 00:00:00 1601 UTC",
  );
  assert(reparseMetadata(epoch).mtime_filetime === "0", "FILETIME epoch conversion failed");
  assert(streams(reparseSample)[0].size === "0", "empty unnamed stream changed");
});

Deno.test("offline file attributes preserve raw flags without FUSE normalization", () => {
  for (const flags of [0, 0x800, 0x420]) {
    const sample = reparseSample.replace("0x00000420", `0x${flags.toString(16).padStart(8, "0")}`);
    assert(fileAttributes(sample) === flags, "DIRECTORY or NORMAL flags were synthesized");
  }
});

Deno.test("offline reparse metadata rejects missing or malformed tool fields", () => {
  for (
    const output of [
      reparseSample.replace("0xa000000c", "missing"),
      reparseSample.replace("0x00000420", "0x00000020"),
      reparseSample.replace("Wed Jan  1 00:00:01 2020 UTC", "Wed Feb 30 00:00:01 2020 UTC"),
      reparseSample.replace("Wed Jan  1 00:00:01 2020 UTC", "Wed Jan  1 00:00:01 2020 PST"),
      blocks(reparseSample, 0x30).join(""),
    ]
  ) {
    let rejected = false;
    try {
      reparseMetadata(output);
    } catch {
      rejected = true;
    }
    assert(rejected, "invalid offline metadata was accepted");
  }
});
