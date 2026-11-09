import { blocks, type Inode, parseInode, partitionRecords, streams } from "./ntfsinfo.ts";
import {
  compactManifest,
  type Entry,
  entryTotals,
  geometry,
  type ManifestInput,
} from "./manifest.ts";

const sample = (name: string) =>
  Deno.readTextFile(new URL(`./testdata/${name}.txt`, import.meta.url));
function assert(condition: boolean, message: string) {
  if (!condition) throw new Error(message);
}
function rejects(action: () => unknown) {
  let rejected = false;
  try {
    action();
  } catch {
    rejected = true;
  }
  assert(rejected, "invalid measurements or inventory were accepted");
}
function entry(inode: Inode, path: string): Entry {
  return { ...inode, path, names: inode.names.map((name) => ({ ...name, parent_seq: 1 })) };
}

Deno.test("captured resident DATA and named streams preserve independently allocated bytes", async () => {
  const values = streams(await sample("ads"));
  assert(values.length === 3, "ADS inventory changed");
  const main = values.find((stream) => stream.name.length === 0)!;
  const big = values.find((stream) => String.fromCharCode(...stream.name) === "big")!;
  const meta = values.find((stream) => String.fromCharCode(...stream.name) === "meta")!;
  assert(
    main.resident && main.allocated_size === "0" && main.compressed_size === null,
    "resident allocation",
  );
  assert(
    big.size === "20480" && big.allocated_size === "20480" && !big.resident,
    "non-resident ADS",
  );
  assert(meta.size === "32" && meta.allocated_size === "0", "resident ADS");
});

Deno.test("captured sparse and compressed physical sizes are not rounded logical sizes", async () => {
  for (
    const [name, size, physical] of [["sparse", "1048576", "8192"], [
      "compressed",
      "262144",
      "32768",
    ]]
  ) {
    const stream = streams(await sample(name))[0];
    assert(stream.size === size && stream.allocated_size === size, `${name} allocated header`);
    assert(stream.compressed_size === physical, `${name} compressed header`);
  }
});

Deno.test("captured BITMAP and INDEX_ALLOCATION appear alongside DATA", async () => {
  const mft = streams(await sample("mft"));
  assert(
    mft.length === 2 &&
      mft.some((stream) => stream.attribute_type === 0xb0 && stream.allocated_size === "4096"),
    "MFT bitmap omitted",
  );
  const index = streams(await sample("index"));
  assert(
    index.length === 1 && index[0].attribute_type === 0xa0 && index[0].allocated_size === "40960",
    "index allocation omitted or resident bitmap included",
  );
});

Deno.test("captured continuation sizes come only from lowest VCN zero and order is irrelevant", async () => {
  const output = await sample("continuation");
  const expected = streams(output);
  const data = expected.find((stream) => stream.attribute_type === 0x80)!;
  assert(
    data.size === "307200" && data.allocated_size === "307200" && data.run_count === 600,
    "continuation stream measurements",
  );
  assert(
    expected.some((stream) => stream.attribute_type === 0x20 && stream.allocated_size === "512"),
    "non-resident list omitted",
  );
  const segments = output.split(/(?=^Dumping attribute )/m).slice(1);
  assert(
    JSON.stringify(streams(segments.reverse().join(""))) === JSON.stringify(expected),
    "segment order changed measurements",
  );
});

Deno.test("stream identity includes type and name and unknown non-resident types are retained", async () => {
  const output = await sample("ads");
  const big = blocks(output, 0x80).find((block) => block.includes("'big'"))!;
  const extra = big.replace("$DATA (0x80)", "$LOGGED_UTILITY_STREAM (0x100)");
  assert(streams(output + extra).length === 4, "same-name different-type stream was merged");
});

Deno.test("missing allocation fields and conflicting or absent first segments fail parsing", async () => {
  const ordinary = blocks(await sample("ads"), 0x80).find((block) => block.includes("'big'"))!;
  rejects(() => streams(ordinary.replace(/^.*Allocated size:.*\n/m, "")));
  rejects(() => streams(ordinary + ordinary));
  rejects(() => streams(ordinary.replace(/Lowest VCN\s+0/, "Lowest VCN 1")));
  for (const name of ["sparse", "compressed"]) {
    const output = await sample(name);
    rejects(() => streams(output.replace(/^.*Compressed size:.*\n/m, "")));
  }
});

Deno.test("captured metafiles include nameless reserved bases and Extend children exactly once", async () => {
  const mft = parseInode(await sample("mft"))!;
  const extend = parseInode(await sample("extend"))!;
  const child = parseInode(await sample("extend-child"))!;
  const reserved = parseInode(await sample("reserved"))!;
  const root: Inode = { ...mft, record: "5", seq: 5, kind: "directory", names: [] };
  const partition = partitionRecords([mft, root, extend, child, reserved], [
    entry(root, "/"),
    entry(mft, "/$MFT"),
  ]);
  assert(partition.entries.length === 1 && partition.metafiles.length === 4, "metadata partition");
  assert(
    partition.metafiles.some((value) => value.record === "12" && value.names.length === 0),
    "nameless reserved record omitted",
  );
  const measuredChild = partition.metafiles.find((value) => value.record === "24")!;
  assert(
    measuredChild.path === "/$Extend/$Quota" && measuredChild.names[0].parent_record === "11" &&
      measuredChild.names[0].parent_seq === extend.seq,
    "Extend parent identity",
  );
  const uncovered = { ...root, record: "999" };
  rejects(() => partitionRecords([mft, root, uncovered], [entry(root, "/")]));
  rejects(() => partitionRecords([root], [entry(mft, "/missing")]));
});

Deno.test("ntfsinfo stderr is validated even when the tool exits successfully", async () => {
  assert(
    parseInode("", "Error loading node: No such file or directory\n", 0) === null,
    "unused/extension inode was treated as live",
  );
  rejects(() => parseInode("", "Error loading node: Input/output error\n", 0));
  rejects(() => parseInode("Dumping Inode 5 (0x5)\n", "", 1));
  const output = await sample("reserved");
  assert(parseInode(output.replace("IN_USE", "")) === null, "deleted record retained");
});

Deno.test("filler compaction sums DATA, ADS and physical allocation before omitting entries", async () => {
  const parent = entry(parseInode(await sample("index"))!, "/fill");
  parent.record = "64";
  parent.seq = 7;
  const children = [parseInode(await sample("ads"))!, parseInode(await sample("sparse"))!].map((
    inode,
    index,
  ): Entry => ({
    ...entry(inode, `/fill/f${index}`),
    record: String(67 + index * 2),
    names: [{ parent_record: "64", parent_seq: 7, name: [102, 48 + index], namespace: 0 }],
  }));
  const input: ManifestInput = {
    schema_version: 3,
    image: "synthetic.img.zst",
    image_len: "16777216",
    generator: { ntfsinfo: "2022.10.3" },
    geometry: geometry(
      "Sector Size: 512\nCluster Size: 4096\nMFT Record Size: 1024\nIndex Block Size: 4096\nVolume Size in Clusters: 4095\nLCN of Data Attribute for FILE_MFT: 4\nLCN of Data Attribute for File_MFTMirr: 2047\n",
    ),
    mft: { extents: [], data_size: "0", has_attribute_list: false },
    entries: [parent, ...children],
    metafiles: [],
    deleted: [],
  };
  const compact = compactManifest(input);
  assert(compact.entries.length === 1 && compact.fill[0].count === 2, "filler count");
  assert(
    compact.fill[0].total_size === "1048576" && compact.fill[0].total_ads_size === "20512" &&
      compact.fill[0].total_alloc === "28672",
    "filler totals",
  );
  rejects(() => compactManifest({ ...input, schema_version: 2 }));
});

Deno.test("BadClus Bad stream logical size is excluded while its physical allocation remains", async () => {
  const value = entry(parseInode(await sample("sparse"))!, "/$BadClus");
  value.record = "8";
  value.streams[0].name = Array.from("$Bad", (c) => c.charCodeAt(0));
  const totals = entryTotals(value);
  assert(
    totals.size === 0n && totals.ads_size === 0n && totals.alloc === 8192n,
    "BadClus accounting",
  );
});

Deno.test("complete Secure headers remain measurable after the captured SDS payload diagnostic", async () => {
  const output = await sample("secure");
  const stderr = await sample("secure.stderr");
  const inode = parseInode(output, stderr)!;
  assert(
    inode.record === "9" && inode.streams[0].allocated_size === "266240",
    "Secure stream header",
  );
  rejects(() => parseInode(output.replace("End of inode reached", "truncated"), stderr));
  rejects(() => parseInode(output, stderr + "unexpected warning\n"));
});

Deno.test("fragmented index payload diagnostics retain first-segment sizes and every run", async () => {
  const output = await sample("fragmented-index");
  const stderr = await sample("fragmented-index.stderr");
  const inode = parseInode(output, stderr)!;
  const index = inode.streams.find((stream) => stream.attribute_type === 0xa0)!;
  assert(inode.record === "64", "index base identity");
  assert(index.size === "14557184" && index.allocated_size === "14557184", "printed index sizes");
  assert(index.compressed_size === null && index.run_count === 513, "index mapping pairs");
  const segments = blocks(output, 0xa0);
  assert(segments.length === 3, "index continuation headers");
  assert(!segments[1].includes("Allocated size:"), "continuation unexpectedly has sizes");
  const changedHeader = output.replace(
    "Allocated size:\t\t 14557184",
    "Allocated size:\t\t 14557696",
  );
  assert(
    parseInode(changedHeader, stderr)!.streams.find((stream) => stream.attribute_type === 0xa0)!
      .allocated_size === "14557696",
    "measurement did not come from the printed header",
  );
  assert(
    parseInode(
      output,
      stderr.replaceAll("Value too large for defined data type", "payload unavailable"),
    )!.record === "64",
    "reason was unnecessarily restricted",
  );
});

Deno.test("payload diagnostics reject mismatched inode numbers and missing affected headers", async () => {
  const output = await sample("fragmented-index");
  const stderr = await sample("fragmented-index.stderr");
  rejects(() => parseInode(output, stderr.replace("in inode 64", "in inode 65")));
  rejects(() => parseInode(output, stderr.replaceAll("0xa0", "0xe0")));
  rejects(() => parseInode(output, stderr, 1));
});

Deno.test("payload diagnostics reject truncated stdout and missing first-segment sizes or mapping pairs", async () => {
  const output = await sample("fragmented-index");
  const stderr = await sample("fragmented-index.stderr");
  rejects(() => parseInode(output.replace("End of inode reached", "truncated"), stderr));
  const first = blocks(output, 0xa0)[0];
  for (const field of ["Data size", "Allocated size", "Initialized size"]) {
    const incomplete = first.replace(new RegExp(`^.*${field}:.*\\n`, "m"), "");
    rejects(() => parseInode(output.replace(first, incomplete), stderr));
  }
  const incomplete = first.replace(/^\s+0x0\s+0x2346\s+0x1002\s*$/m, "");
  rejects(() => parseInode(output.replace(first, incomplete), stderr));
  rejects(() => parseInode(output + first, stderr));
});

Deno.test("payload diagnostics reject unknown stderr and incomplete diagnostic pairs", async () => {
  const output = await sample("fragmented-index");
  const stderr = await sample("fragmented-index.stderr");
  rejects(() => parseInode(output, "unexpected tool diagnostic\n"));
  rejects(() => parseInode(output, stderr + "unexpected tool diagnostic\n"));
  rejects(() => parseInode(output, "Corrupt attribute 0xa0 in inode 64\n"));
});
