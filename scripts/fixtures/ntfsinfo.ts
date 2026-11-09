import type { Entry, Extent, Geometry, Name, Stream } from "./manifest.ts";

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
    (block.match(/^\s*Runlist:[^\n]*\n((?:[ \t]+0x[^\n]*\n?)*)/m)?.[1] ?? "").matchAll(
      /^\s+0x([\da-f]+)\s+(0x[\da-f]+|<HOLE>)\s+0x([\da-f]+)\s*$/gim,
    ),
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
  const groups = new Map<string, { type: number; name: number[]; blocks: string[] }>();
  for (const block of output.split(/(?=^Dumping attribute )/m)) {
    const type = block.match(/^Dumping attribute \$?\w+ \(0x([\da-f]+)\)/i);
    if (!type) continue;
    const attribute_type = parseInt(type[1], 16);
    const storage = block.match(/^\s*Resident:\s*(Yes|No)\s*$/m)?.[1];
    if (!storage) throw new Error("ntfsinfo is missing storage form");
    if (attribute_type !== 0x80 && storage === "Yes") continue;
    const name = units(block.match(/^\s*Attribute name:\s*'(.*)'\s*$/m)?.[1] ?? "");
    const key = JSON.stringify([attribute_type, name]);
    const group = groups.get(key) ?? { type: attribute_type, name, blocks: [] };
    group.blocks.push(block);
    groups.set(key, group);
  }
  return Array.from(groups.values(), (group) => {
    const resident = /^\s*Resident:\s*Yes\s*$/m.test(group.blocks[0]);
    if (group.blocks.some((block) => /^\s*Resident:\s*Yes\s*$/m.test(block) !== resident)) {
      throw new Error("mixed stream storage forms");
    }
    const first = resident
      ? group.blocks
      : group.blocks.filter((block) => integer(block, "Lowest VCN") === "0");
    if (first.length !== 1 || (resident && group.blocks.length !== 1)) {
      throw new Error("stream must have exactly one first segment");
    }
    const block = first[0];
    const flags = block.match(/^\s*Attribute flags:\s*0x([\da-f]+)/im);
    if (!flags) throw new Error("ntfsinfo is missing attribute flags");
    const bits = parseInt(flags[1], 16);
    const sparse = (bits & 0x8000) !== 0;
    const compressed = (bits & 0xff) !== 0;
    return {
      attribute_type: group.type,
      name: group.name,
      size: integer(block, "Data size"),
      allocated_size: resident ? "0" : integer(block, "Allocated size"),
      compressed_size: !resident && (sparse || compressed)
        ? integer(block, "Compressed size")
        : null,
      resident,
      sparse,
      compressed,
      run_count: group.blocks.reduce((count, segment) => count + extents(segment).length, 0),
    };
  }).sort((a, b) => a.attribute_type - b.attribute_type || compareNames(a.name, b.name));
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
  const mtime = filetime(altered);
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

function filetime(altered: RegExpMatchArray): bigint {
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
  return mtime;
}

export type Inode = Omit<Entry, "path" | "names"> & {
  names: Omit<Name, "parent_seq">[];
};

function validatePayloadDiagnostics(output: string, stderr: string) {
  const inode = integer(output, "Dumping Inode");
  const end = output.search(/^End of inode reached\r?$/m);
  if (end < 0 || /^Dumping attribute /m.test(output.slice(end))) {
    throw new Error("ntfsinfo payload diagnostic has incomplete stdout");
  }
  const lines = stderr.trimEnd().split(/\r?\n/);
  if (lines.length % 2 !== 0) throw new Error(`unrecognized ntfsinfo stderr: ${stderr}`);
  const affected = new Set<number>();
  for (let index = 0; index < lines.length; index += 2) {
    const corrupt = lines[index].match(/^Corrupt attribute 0x([\da-fA-F]+) in inode (\d+)$/);
    const failed = lines[index + 1].match(
      /^Failed to read \$[A-Za-z_][\w]* attribute: (\S[^\r\n]*)$/,
    );
    if (!corrupt || !failed || BigInt(corrupt[2]).toString() !== inode) {
      throw new Error(`unrecognized or mismatched ntfsinfo stderr: ${stderr}`);
    }
    affected.add(parseInt(corrupt[1], 16));
  }
  for (const type of affected) {
    const attributes = blocks(output, type);
    if (attributes.length === 0) {
      throw new Error(`missing diagnostic attribute 0x${type.toString(16)}`);
    }
    for (const attribute of attributes) {
      const resident = attribute.match(/^\s*Resident:\s*(Yes|No)\s*$/m)?.[1];
      if (resident === "Yes") {
        integer(attribute, "Data size");
        continue;
      }
      if (resident !== "No") throw new Error("missing diagnostic attribute storage form");
      const lowest = BigInt(integer(attribute, "Lowest VCN"));
      const highest = attribute.match(/^\s*Highest VCN:\s*(-?\d+)\b/m)?.[1];
      if (highest === undefined || !/^\s*Runlist:/m.test(attribute)) {
        throw new Error("missing diagnostic attribute mapping pairs");
      }
      if (lowest === 0n) {
        const data = BigInt(integer(attribute, "Data size"));
        integer(attribute, "Allocated size");
        if (BigInt(integer(attribute, "Initialized size")) > data) {
          throw new Error("invalid diagnostic attribute initialized size");
        }
        const flags = attribute.match(/^\s*Attribute flags:\s*0x([\da-f]+)/im)?.[1];
        if (flags === undefined) throw new Error("missing diagnostic attribute flags");
        if ((parseInt(flags, 16) & 0x80ff) !== 0) integer(attribute, "Compressed size");
      }
      let next = lowest;
      for (const run of extents(attribute)) {
        if (BigInt(run.vcn) !== next || BigInt(run.len) === 0n) {
          throw new Error("incomplete diagnostic attribute mapping pairs");
        }
        next += BigInt(run.len);
      }
      if (next !== BigInt(highest) + 1n) {
        throw new Error("incomplete diagnostic attribute mapping pairs");
      }
    }
  }
}

// ntfsinfo 2022.10.3 reports unavailable/free/extension inodes on stderr even with exit 0.
export function parseInode(output: string, stderr = "", code = 0): Inode | null {
  if (
    (code === 0 || code === 1) && !output.trim() &&
    /^Error loading node: No such file or directory\s*$/.test(stderr)
  ) {
    return null;
  }
  if (code !== 0) throw new Error(`ntfsinfo failed (${code}): ${stderr}`);
  if (stderr.trim()) validatePayloadDiagnostics(output, stderr);
  const flags = output.match(/^MFT Record Flags:\s*([^\n]*)/m)?.[1];
  if (flags === undefined) throw new Error("ntfsinfo is missing record flags");
  if (!/\bIN_USE\b/.test(flags)) return null;
  const names = blocks(output, 0x30).map((block) => {
    const namespace = block.match(/^\s*Namespace:\s*(.+)$/m)?.[1].trim();
    const namespaces: Record<string, number> = { POSIX: 0, Win32: 1, DOS: 2, "Win32 & DOS": 3 };
    const name = block.match(/^\s*Filename:\s*'(.*)'\s*$/m)?.[1];
    if (namespace === undefined || namespaces[namespace] === undefined || name === undefined) {
      throw new Error("invalid FILE_NAME output");
    }
    return {
      parent_record: integer(block, "Parent directory"),
      name: units(name),
      namespace: namespaces[namespace],
    };
  });
  const standard = blocks(output, 0x10)[0];
  const altered = standard?.match(
    /^\s*File Altered Time:\s*\w{3} (\w{3})\s+(\d{1,2}) (\d{2}:\d{2}:\d{2}) (\d{4}) UTC\s*$/m,
  );
  if (!altered) throw new Error("ntfsinfo is missing STANDARD_INFORMATION modified time");
  const reparse = blocks(output, 0xc0)[0];
  const tag = reparse?.match(/^\s*Reparse tag:\s*0x([\da-f]{8})\b/im)?.[1];
  if (reparse !== undefined && tag === undefined) {
    throw new Error("ntfsinfo is missing reparse tag");
  }
  const reparse_tag = tag === undefined ? null : parseInt(tag, 16);
  return {
    record: integer(output, "Dumping Inode"),
    seq: Number(integer(output, "MFT Record Seq. Numb.")),
    nlink: Number(integer(output, "Number of Hard Links")),
    kind: /\bDIRECTORY\b/.test(flags)
      ? "directory"
      : reparse_tag === 0xa000000c
      ? "symlink"
      : "file",
    attributes: fileAttributes(output),
    mtime_filetime: filetime(altered).toString(),
    names,
    streams: streams(output),
    reparse_tag,
    has_attribute_list: blocks(output, 0x20).length !== 0,
  };
}

export function partitionRecords(
  records: readonly Inode[],
  collected: readonly Entry[],
): { entries: Entry[]; metafiles: Entry[] } {
  const byRecord = new Map(records.map((record) => [record.record, record]));
  if (byRecord.size !== records.length) throw new Error("duplicate census record");
  const isMetadata = (record: Inode) =>
    record.record !== "5" &&
    (BigInt(record.record) < 16n || record.names.some((name) => name.parent_record === "11"));
  const measured = (record: Inode, path: string): Entry => ({
    ...record,
    path,
    names: record.names.map((name) => {
      const parent = byRecord.get(name.parent_record);
      if (!parent) throw new Error(`missing parent ${name.parent_record} of ${record.record}`);
      return { ...name, parent_seq: parent.seq };
    }),
  });
  const entries = collected.flatMap((entry) => {
    const record = byRecord.get(entry.record);
    if (!record) throw new Error(`collected entry is not a live base: ${entry.path}`);
    if (isMetadata(record)) return [];
    const value = measured(record, entry.path);
    // Preserve the mount's full FILETIME resolution except after the deferred reparse write.
    if (record.reparse_tag === null) value.mtime_filetime = entry.mtime_filetime;
    return [value];
  });
  const metafiles = records.filter(isMetadata).map((record) => {
    const name = [...record.names].sort((a, b) => a.namespace - b.namespace)[0];
    const path = name
      ? `${name.parent_record === "11" ? "/$Extend" : ""}/${String.fromCharCode(...name.name)}`
      : `/<record ${record.record}>`;
    return measured(record, path);
  });
  const covered = new Set([...entries, ...metafiles].map((entry) => entry.record));
  for (const record of records) {
    if (!covered.has(record.record)) throw new Error(`uncovered live base record ${record.record}`);
  }
  return { entries, metafiles };
}

function compareNames(a: readonly number[], b: readonly number[]): number {
  for (let i = 0; i < Math.min(a.length, b.length); i++) {
    if (a[i] !== b[i]) return a[i] - b[i];
  }
  return a.length - b.length;
}
