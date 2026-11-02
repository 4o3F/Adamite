import { type DeletedRecord, getAttribute, paths, text } from "./lib.ts";

export type Extent = { vcn: string; lcn: string | null; len: string };
export type Name = { parent_record: string; parent_seq: number; name: number[]; namespace: number };
export type Stream = {
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
  schema_version: 2;
  image: string;
  image_len: string;
  generator: Record<string, string>;
  geometry: Geometry;
  mft: { extents: Extent[]; data_size: string; has_attribute_list: boolean };
  entries: Entry[];
  fill: Fill[];
  deleted: DeletedRecord[];
};

export type Fill = {
  dir: string;
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
  if (input.schema_version !== 1 && input.schema_version !== 2) {
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
    fill.push({
      dir,
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
    schema_version: 2,
    image: input.image,
    image_len: input.image_len,
    generator: input.generator,
    geometry: input.geometry,
    mft: input.mft,
    entries,
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

export function units(value: string): number[] {
  return Array.from({ length: value.length }, (_, index) => value.charCodeAt(index));
}

export function integer(output: string, label: string): string {
  const escaped = label.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const match = output.match(new RegExp(`^\\s*${escaped}:?\\s+([0-9]+)\\b`, "m"));
  if (!match) throw new Error(`ntfsinfo is missing ${label}`);
  return BigInt(match[1]).toString();
}

export function blocks(output: string, type: number): string[] {
  return output.split(/(?=^Dumping attribute )/m).filter((block) => {
    const match = block.match(/^Dumping attribute \$?\w+ \(0x([\da-f]+)\)/i);
    return match !== null && parseInt(match[1], 16) === type;
  });
}

export function extents(block: string): Extent[] {
  return Array.from(
    block.matchAll(/^\s+0x([\da-f]+)\s+(0x[\da-f]+|<HOLE>)\s+0x([\da-f]+)\s*$/gim),
    (match) => ({
      vcn: BigInt(`0x${match[1]}`).toString(),
      lcn: match[2] === "<HOLE>" ? null : BigInt(match[2]).toString(),
      len: BigInt(`0x${match[3]}`).toString(),
    }),
  );
}

export function geometry(output: string): Geometry {
  return {
    bytes_per_sector: Number(integer(output, "Sector Size")),
    cluster_size: Number(integer(output, "Cluster Size")),
    file_record_size: Number(integer(output, "MFT Record Size")),
    index_record_size: Number(integer(output, "Index Block Size")),
    total_clusters: integer(output, "Volume Size in Clusters"),
    mft_lcn: integer(output, "LCN of Data Attribute for FILE_MFT"),
    mftmirr_lcn: integer(output, "LCN of Data Attribute for File_MFTMirr"),
  };
}

export function streams(output: string): Stream[] {
  const result = new Map<string, Stream>();
  for (const block of blocks(output, 0x80)) {
    const name = units(block.match(/^\s*Attribute name:\s*'(.*)'\s*$/m)?.[1] ?? "");
    const key = JSON.stringify(name);
    const flags = block.match(/^\s*Attribute flags:\s*0x([\da-f]+)/im);
    if (!flags) throw new Error("ntfsinfo is missing attribute flags");
    const bits = parseInt(flags[1], 16);
    const resident = /^\s*Resident:\s*Yes\s*$/m.test(block);
    const previous = result.get(key);
    const size = /^\s*Data size:/m.test(block) ? integer(block, "Data size") : previous?.size;
    if (size === undefined) throw new Error("first DATA segment has no data size");
    result.set(key, {
      name,
      size,
      resident,
      sparse: (bits & 0x8000) !== 0,
      compressed: (bits & 0xff) !== 0,
      run_count: (previous?.run_count ?? 0) + extents(block).length,
    });
  }
  return Array.from(result.values());
}

export function fileAttributes(output: string): number {
  const standard = blocks(output, 0x10)[0];
  const attributes = standard?.match(/^\s*File attributes:.*\(0x([\da-f]{8})\)\s*$/im);
  if (!attributes) throw new Error("ntfsinfo is missing STANDARD_INFORMATION file attributes");
  return parseInt(attributes[1], 16);
}

export function reparseMetadata(
  output: string,
): Pick<Entry, "attributes" | "mtime_filetime" | "reparse_tag" | "kind"> {
  const standard = blocks(output, 0x10)[0];
  const reparse = blocks(output, 0xc0)[0];
  if (standard === undefined || reparse === undefined) {
    throw new Error("ntfsinfo is missing STANDARD_INFORMATION or REPARSE_POINT");
  }
  const tag = reparse.match(/^\s*Reparse tag:\s*0x([\da-f]{8})\b/im);
  const altered = standard.match(
    /^\s*File Altered Time:\s*\w{3} (\w{3})\s+(\d{1,2}) (\d{2}:\d{2}:\d{2}) (\d{4}) UTC\s*$/m,
  );
  if (!tag || !altered) throw new Error("invalid ntfsinfo reparse metadata");
  const months = [
    "Jan",
    "Feb",
    "Mar",
    "Apr",
    "May",
    "Jun",
    "Jul",
    "Aug",
    "Sep",
    "Oct",
    "Nov",
    "Dec",
  ];
  const month = months.indexOf(altered[1]) + 1;
  const iso = `${altered[4]}-${month.toString().padStart(2, "0")}-${altered[2].padStart(2, "0")}T${
    altered[3]
  }.000Z`;
  const milliseconds = Date.parse(iso);
  if (!Number.isFinite(milliseconds) || new Date(milliseconds).toISOString() !== iso) {
    throw new Error("invalid ntfsinfo reparse mtime");
  }
  const mtime = BigInt(milliseconds) * 10000n + 116444736000000000n;
  if (mtime < 0n) throw new Error("ntfsinfo reparse mtime precedes the FILETIME epoch");
  const flags = fileAttributes(output);
  if ((flags & 0x400) === 0) throw new Error("ntfsinfo reparse file is missing REPARSE_POINT flag");
  const reparse_tag = parseInt(tag[1], 16);
  return {
    attributes: flags,
    mtime_filetime: mtime.toString(),
    reparse_tag,
    kind: reparse_tag === 0xa000000c ? "symlink" : (flags & 0x10) !== 0 ? "directory" : "file",
  };
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

export async function finalize(
  imagePath: string,
  imageName: string,
  imageLength: bigint,
  generator: Record<string, string>,
  entries: Entry[],
  deleted: DeletedRecord[],
  reparsePaths: readonly string[] = [],
): Promise<Manifest> {
  const cache = new Map<string, Promise<string>>();
  const inode = (record: string): Promise<string> => {
    let value = cache.get(record);
    if (!value) {
      value = text("ntfsinfo", ["-v", "-i", record, imagePath]);
      cache.set(record, value);
    }
    return value;
  };
  for (const path of reparsePaths) {
    if (!entries.some((entry) => entry.path === path)) {
      throw new Error(`missing collected reparse entry ${path}`);
    }
  }
  for (const entry of entries) {
    const output = reparsePaths.includes(entry.path)
      ? await text("ntfsinfo", ["-v", "-F", entry.path, imagePath])
      : await inode(entry.record);
    // The mount xattr synthesizes DIRECTORY/NORMAL bits; preserve the on-disk SI flags.
    entry.attributes = fileAttributes(output);
    if (reparsePaths.includes(entry.path)) {
      if (integer(output, "Dumping Inode") !== entry.record) {
        throw new Error(`reparse record changed for ${entry.path}`);
      }
      Object.assign(entry, reparseMetadata(output));
    }
    entry.seq = Number(integer(output, "MFT Record Seq. Numb."));
    entry.has_attribute_list = blocks(output, 0x20).length !== 0;
    entry.streams = streams(output);
    for (const block of blocks(output, 0x30)) {
      const parent_record = integer(block, "Parent directory");
      const namespace = block.match(/^\s*Namespace:\s*(.+)$/m)?.[1].trim();
      const namespaces: Record<string, number> = { POSIX: 0, Win32: 1, DOS: 2, "Win32 & DOS": 3 };
      const name = block.match(/^\s*Filename:\s*'(.*)'\s*$/m)?.[1];
      if (namespace === undefined || namespaces[namespace] === undefined || name === undefined) {
        throw new Error(`invalid FILE_NAME output for ${entry.path}`);
      }
      entry.names.push({
        parent_record,
        parent_seq: Number(integer(await inode(parent_record), "MFT Record Seq. Numb.")),
        name: units(name),
        namespace: namespaces[namespace],
      });
    }
  }
  const mft = await inode("0");
  const data = blocks(mft, 0x80).filter((block) => !/^\s*Attribute name:/m.test(block));
  if (data.length === 0) throw new Error("MFT has no unnamed DATA");
  const result: Manifest = {
    schema_version: 2,
    image: `${imageName}.img.zst`,
    image_len: imageLength.toString(),
    generator,
    geometry: geometry(await text("ntfsinfo", ["-m", imagePath])),
    mft: {
      extents: data.flatMap(extents),
      data_size: integer(data[0], "Data size"),
      has_attribute_list: blocks(mft, 0x20).length !== 0,
    },
    entries,
    fill: [],
    deleted,
  };
  if (result.mft.extents.length === 0 || result.mft.extents.some((run) => run.lcn === null)) {
    throw new Error("MFT has missing or sparse physical extents");
  }
  return compactManifest(result);
}
