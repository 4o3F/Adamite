import { blocks, extents, geometry, parseInode, partitionRecords } from "./ntfsinfo.ts";
export {
  blocks,
  extents,
  fileAttributes,
  geometry,
  integer,
  reparseMetadata,
  streams,
  units,
} from "./ntfsinfo.ts";

import { command, type DeletedRecord, getAttribute, paths, text } from "./lib.ts";

export type Extent = { vcn: string; lcn: string | null; len: string };
export type Name = { parent_record: string; parent_seq: number; name: number[]; namespace: number };
export type Stream = {
  attribute_type: number;
  allocated_size: string;
  compressed_size: string | null;
  name: number[];
  size: string;
  resident: boolean;
  sparse: boolean;
  compressed: boolean;
  run_count: number;
};
export type Entry = {
  path: string;
  record: string;
  seq: number;
  kind: "directory" | "file" | "symlink";
  nlink: number;
  attributes: number;
  mtime_filetime: string;
  names: Name[];
  streams: Stream[];
  reparse_tag: number | null;
  has_attribute_list: boolean;
};
export type Geometry = {
  bytes_per_sector: number;
  cluster_size: number;
  file_record_size: number;
  index_record_size: number;
  total_clusters: string;
  mft_lcn: string;
  mftmirr_lcn: string;
};
export type Manifest = {
  schema_version: 3;
  image: string;
  image_len: string;
  generator: Record<string, string>;
  geometry: Geometry;
  mft: { extents: Extent[]; data_size: string; has_attribute_list: boolean };
  entries: Entry[];
  metafiles: Entry[];
  fill: Fill[];
  deleted: DeletedRecord[];
};

export type Fill = {
  dir: string;
  total_size: string;
  total_ads_size: string;
  total_alloc: string;
  count: number;
  name_prefix: string;
  name_len: number;
  record_min: string;
  record_max: string;
  parent_record: string;
  parent_seq: number;
};

export type ManifestInput = Omit<Manifest, "schema_version" | "fill"> & {
  schema_version: number;
  fill?: Fill[];
};

export function compactManifest(input: ManifestInput): Manifest {
  if (input.schema_version !== 3) {
    throw new Error(`unsupported manifest schema ${input.schema_version}`);
  }
  const fill = [...(input.fill ?? [])];
  let entries = [...input.entries];
  for (const dir of ["/fill", "/growth"]) {
    const children = entries.filter((entry) => entry.path.startsWith(`${dir}/`));
    if (children.length === 0) continue;
    if (fill.some((set) => set.dir === dir)) throw new Error(`duplicate filler summary ${dir}`);
    const parent = entries.find((entry) => entry.path === dir);
    if (!parent) throw new Error(`missing filler directory ${dir}`);
    const nameLength = children[0].names[0]?.name.length;
    const records = new Set<string>();
    for (const entry of children) {
      const name = entry.names[0];
      if (
        entry.kind !== "file" || entry.names.length !== 1 ||
        name.name.length !== nameLength || name.name[0] !== 102 ||
        name.parent_record !== parent.record || name.parent_seq !== parent.seq ||
        entry.path !== `${dir}/${String.fromCharCode(...name.name)}` || records.has(entry.record)
      ) {
        throw new Error(`invalid filler entry ${entry.path}`);
      }
      records.add(entry.record);
    }
    const ordered = [...records].map(BigInt).sort((a, b) => a < b ? -1 : a > b ? 1 : 0);
    const totals = children.map(entryTotals);
    fill.push({
      dir,
      total_size: totals.reduce((sum, item) => sum + item.size, 0n).toString(),
      total_ads_size: totals.reduce((sum, item) => sum + item.ads_size, 0n).toString(),
      total_alloc: totals.reduce((sum, item) => sum + item.alloc, 0n).toString(),
      count: children.length,
      name_prefix: "f",
      name_len: nameLength!,
      record_min: ordered[0].toString(),
      record_max: ordered.at(-1)!.toString(),
      parent_record: parent.record,
      parent_seq: parent.seq,
    });
    entries = entries.filter((entry) => !entry.path.startsWith(`${dir}/`));
  }
  entries.sort((a, b) => a.path < b.path ? -1 : a.path > b.path ? 1 : 0);
  fill.sort((a, b) => a.dir < b.dir ? -1 : a.dir > b.dir ? 1 : 0);
  return {
    schema_version: 3,
    image: input.image,
    image_len: input.image_len,
    generator: input.generator,
    geometry: input.geometry,
    mft: input.mft,
    entries,
    metafiles: [...input.metafiles].sort((a, b) => Number(BigInt(a.record) - BigInt(b.record))),
    fill,
    deleted: input.deleted,
  };
}

export function json(value: unknown): string {
  const normalized = JSON.parse(
    JSON.stringify(value, (_, item) => typeof item === "bigint" ? item.toString() : item),
  );
  function format(item: unknown, depth: number): string {
    const indent = "  ".repeat(depth);
    if (Array.isArray(item)) {
      if (item.every((child) => typeof child === "number")) return JSON.stringify(item);
      return `[\n${
        item.map((child) => `${indent}  ${JSON.stringify(child)}`).join(",\n")
      }\n${indent}]`;
    }
    if (item !== null && typeof item === "object") {
      return `{\n${
        Object.entries(item).map(([key, child]) =>
          `${indent}  ${JSON.stringify(key)}: ${format(child, depth + 1)}`
        ).join(",\n")
      }\n${indent}}`;
    }
    return JSON.stringify(item);
  }
  return format(normalized, 0) + "\n";
}

export async function collect(root: string): Promise<Entry[]> {
  const entries: Entry[] = [];
  for (const path of await paths(root)) {
    const stat = await Deno.lstat(path);
    if (stat.ino === null || stat.nlink === null) {
      throw new Error(`missing inode metadata: ${path}`);
    }
    const attributes = await getAttribute(path, "system.ntfs_attrib_be");
    const times = await getAttribute(path, "system.ntfs_times_be");
    if (attributes.length !== 4 || times.length !== 32) {
      throw new Error(`invalid NTFS xattrs: ${path}`);
    }
    const flags = new DataView(attributes.buffer, attributes.byteOffset, attributes.byteLength)
      .getUint32(0, false);
    let reparse_tag: number | null = null;
    if ((flags & 0x400) !== 0) {
      const value = await getAttribute(path, "system.ntfs_reparse_data");
      if (value.length < 8) throw new Error(`invalid reparse xattr: ${path}`);
      reparse_tag = new DataView(value.buffer, value.byteOffset, value.byteLength).getUint32(
        0,
        true,
      );
    }
    entries.push({
      path: path === root ? "/" : path.slice(root.length),
      record: stat.ino.toString(),
      seq: 0,
      kind: stat.isDirectory ? "directory" : stat.isSymlink ? "symlink" : "file",
      nlink: stat.nlink,
      attributes: flags,
      mtime_filetime: new DataView(times.buffer, times.byteOffset, times.byteLength).getBigUint64(
        8,
        false,
      ).toString(),
      names: [],
      streams: [],
      reparse_tag,
      has_attribute_list: false,
    });
  }
  return entries;
}

export function entryTotals(entry: Entry): { size: bigint; ads_size: bigint; alloc: bigint } {
  let size = 0n;
  let ads_size = 0n;
  let alloc = 0n;
  for (const stream of entry.streams) {
    if (stream.attribute_type === 0x80) {
      if (stream.name.length === 0) size += BigInt(stream.size);
      else if (!(entry.record === "8" && String.fromCharCode(...stream.name) === "$Bad")) {
        ads_size += BigInt(stream.size);
      }
    }
    if (!stream.resident) {
      const measured = stream.sparse || stream.compressed
        ? stream.compressed_size
        : stream.allocated_size;
      if (measured === null) throw new Error(`missing physical size on ${entry.path}`);
      alloc += BigInt(measured);
    }
  }
  return { size, ads_size, alloc };
}

export async function finalize(
  imagePath: string,
  imageName: string,
  imageLength: bigint,
  generator: Record<string, string>,
  collected: Entry[],
  deleted: DeletedRecord[],
  reparsePaths: readonly string[] = [],
): Promise<Manifest> {
  for (const path of reparsePaths) {
    if (!collected.some((entry) => entry.path === path)) {
      throw new Error(`missing collected reparse entry ${path}`);
    }
  }
  const inode = async (record: string) => {
    const result = await command("ntfsinfo", ["-v", "-i", record, imagePath], [0, 1]);
    const stdout = new TextDecoder().decode(result.stdout);
    const parsed = parseInode(stdout, new TextDecoder().decode(result.stderr), result.code);
    if (parsed && result.stderr.length) {
      console.warn(new TextDecoder().decode(result.stderr).trim());
    }
    if (parsed && parsed.record !== record) {
      throw new Error(`ntfsinfo returned wrong record for ${record}`);
    }
    return { stdout, parsed };
  };
  const volume = geometry(await text("ntfsinfo", ["-m", imagePath]));
  const first = await inode("0");
  if (!first.parsed) throw new Error("MFT record zero is unavailable");
  const data = blocks(first.stdout, 0x80).filter((block) => !/^\s*Attribute name:/m.test(block));
  const stream = first.parsed.streams.find((stream) =>
    stream.attribute_type === 0x80 && stream.name.length === 0
  );
  if (
    !stream || stream.resident || stream.sparse || stream.compressed ||
    BigInt(stream.size) === 0n || BigInt(stream.size) > imageLength ||
    BigInt(stream.size) % BigInt(volume.file_record_size) !== 0n
  ) {
    throw new Error("invalid MFT DATA length or storage");
  }
  const count = BigInt(stream.size) / BigInt(volume.file_record_size);
  const records = [first.parsed];
  for (let record = 1n; record < count; record++) {
    const value = await inode(record.toString());
    if (value.parsed) records.push(value.parsed);
  }
  const partition = partitionRecords(records, collected);
  const result: Manifest = {
    schema_version: 3,
    image: `${imageName}.img.zst`,
    image_len: imageLength.toString(),
    generator,
    geometry: volume,
    mft: {
      extents: data.flatMap(extents),
      data_size: stream.size,
      has_attribute_list: first.parsed.has_attribute_list,
    },
    ...partition,
    fill: [],
    deleted,
  };
  if (result.mft.extents.length === 0 || result.mft.extents.some((run) => run.lcn === null)) {
    throw new Error("MFT has missing or sparse physical extents");
  }
  return compactManifest(result);
}
