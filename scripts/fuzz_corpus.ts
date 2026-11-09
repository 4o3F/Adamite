import { type Manifest } from "./fixtures/manifest.ts";

const targets = ["boot_sector", "file_record", "attributes", "runlist", "typed_values"] as const;
type Target = typeof targets[number];
type Seed = { bytes: Uint8Array; hash: string };
const budget = 64 * 1024;
const destination = "crates/adamite-ntfs/fuzz/corpus";

function view(bytes: Uint8Array): DataView {
  return new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
}
function concatenate(...parts: Uint8Array[]): Uint8Array {
  const result = new Uint8Array(parts.reduce((sum, part) => sum + part.length, 0));
  let offset = 0;
  for (const part of parts) {
    result.set(part, offset);
    offset += part.length;
  }
  return result;
}
function fixedRecord(raw: Uint8Array): Uint8Array | null {
  if (new TextDecoder().decode(raw.subarray(0, 4)) !== "FILE" || raw.length < 512) return null;
  const header = view(raw);
  const offset = header.getUint16(4, true);
  const count = header.getUint16(6, true);
  const first = header.getUint16(20, true);
  const used = header.getUint32(24, true);
  if (
    count !== raw.length / 512 + 1 || offset < 42 || offset % 2 !== 0 ||
    offset + count * 2 > Math.min(first, 510) || used > raw.length || first >= used
  ) return null;
  const result = raw.slice();
  const data = view(result);
  const usn = header.getUint16(offset, true);
  for (let stride = 1; stride < count; stride++) {
    const tail = stride * 512 - 2;
    if (header.getUint16(tail, true) !== usn) return null;
    data.setUint16(tail, header.getUint16(offset + stride * 2, true), true);
  }
  return result;
}

function* attributes(record: Uint8Array): Generator<Uint8Array> {
  const header = view(record);
  let offset = header.getUint16(20, true);
  const used = header.getUint32(24, true);
  while (offset + 4 <= used && header.getUint32(offset, true) !== 0xffffffff) {
    if (offset + 24 > used) throw new Error("truncated fixture attribute");
    const size = header.getUint32(offset + 4, true);
    if (size < 24 || size % 8 !== 0 || offset + size > used) {
      throw new Error("invalid fixture attribute length");
    }
    yield record.subarray(offset, offset + size);
    offset += size;
  }
}

function* mappingPairs(attribute: Uint8Array) {
  const data = view(attribute);
  let offset = data.getUint16(32, true);
  let vcn = Number(data.getBigUint64(16, true));
  let lcn = 0n;
  while (offset < attribute.length) {
    const header = attribute[offset++];
    if (header === 0) return;
    const lengthWidth = header & 15;
    const deltaWidth = header >> 4;
    if (
      lengthWidth < 1 || lengthWidth > 8 || deltaWidth > 8 ||
      offset + lengthWidth + deltaWidth > attribute.length
    ) throw new Error("invalid mapping pair");
    let length = 0n;
    for (let index = 0; index < lengthWidth; index++) {
      length |= BigInt(attribute[offset++]) << BigInt(8 * index);
    }
    let delta = 0n;
    for (let index = 0; index < deltaWidth; index++) {
      delta |= BigInt(attribute[offset++]) << BigInt(8 * index);
    }
    if (deltaWidth > 0 && (attribute[offset - 1] & 128) !== 0) {
      delta -= 1n << BigInt(8 * deltaWidth);
    }
    if (deltaWidth > 0) lcn += delta;
    if (
      length <= 0n || length > BigInt(Number.MAX_SAFE_INTEGER) || lcn < 0n ||
      lcn > BigInt(Number.MAX_SAFE_INTEGER)
    ) throw new Error("fixture mapping pair exceeds range");
    yield { vcn, lcn: deltaWidth === 0 ? null : Number(lcn), length: Number(length) };
    vcn += Number(length);
  }
  throw new Error("missing mapping-pairs terminator");
}

function value(attribute: Uint8Array, image: Uint8Array, cluster: number): Uint8Array | null {
  const data = view(attribute);
  if (attribute[8] === 0) {
    const offset = data.getUint16(20, true);
    return attribute.slice(offset, offset + data.getUint32(16, true));
  }
  if (data.getBigUint64(16, true) !== 0n) return null;
  const size = Number(data.getBigUint64(48, true));
  if (size > budget - 16) return null;
  const result = new Uint8Array(size);
  for (const run of mappingPairs(attribute)) {
    const offset = run.vcn * cluster;
    if (offset >= size) break;
    const length = Math.min(size - offset, run.length * cluster);
    if (run.lcn !== null) {
      const start = run.lcn * cluster;
      if (start + length > image.length) throw new Error("stream outside image");
      result.set(image.subarray(start, start + length), offset);
    }
  }
  return result;
}

async function main() {
  const selectorSource = await Deno.readTextFile(
    "crates/adamite-ntfs/fuzz/fuzz_targets/typed_values/selectors.rs",
  );
  const selectors = Object.fromEntries(Array.from(
    selectorSource.matchAll(/^pub const ([A-Z][A-Z0-9_]*): u8 = ([0-9]+);$/gm),
    (match) => [match[1], Number(match[2])],
  ));
  const selectorNames = [
    "STANDARD_INFORMATION",
    "FILE_NAME",
    "REPARSE_POINT",
    "ATTRIBUTE_LIST",
    "BITMAP",
    "UTF16",
    "DATA",
    "INDEX_ALLOCATION",
  ];
  const codes = selectorNames.map((name) => selectors[name]);
  if (
    selectors.COUNT !== selectorNames.length || new Set(codes).size !== codes.length ||
    codes.some((code) => !Number.isInteger(code) || code < 0 || code >= selectors.COUNT)
  ) {
    throw new Error("invalid Rust typed-value selector table");
  }
  const seen = new Map<Target, Set<string>>(targets.map((target) => [target, new Set()]));
  const images: Map<Target, Seed[]>[] = [];
  for (const name of ["basic-4k", "frag-512", "4kn-64k"]) {
    const root = "tests/fixtures/ntfs";
    const manifest: Omit<Manifest, "schema_version"> & { schema_version: number } = JSON.parse(
      await Deno.readTextFile(`${root}/${name}.json`),
    );
    if (manifest.schema_version !== 2 && manifest.schema_version !== 3) {
      throw new Error("seed extraction requires manifest schema 2 or 3");
    }
    const decoded = await new Deno.Command("zstd", {
      args: ["-d", "--stdout", `${root}/${manifest.image}`],
      stdout: "piped",
      stderr: "piped",
    }).output();
    if (!decoded.success) throw new Error(new TextDecoder().decode(decoded.stderr));
    const image = decoded.stdout;
    if (BigInt(image.length) !== BigInt(manifest.image_len)) {
      throw new Error("image length mismatch");
    }
    const seeds = new Map<Target, Seed[]>(targets.map((target) => [target, []]));
    images.push(seeds);
    const add = async (target: Target, bytes: Uint8Array) => {
      if (bytes.length === 0 || bytes.length > budget) return;
      const hash = Array.from(
        new Uint8Array(await crypto.subtle.digest("SHA-256", bytes.slice())),
        (byte) => byte.toString(16).padStart(2, "0"),
      ).join("");
      if (seen.get(target)!.has(hash)) return;
      seen.get(target)!.add(hash);
      seeds.get(target)!.push({ bytes: bytes.slice(), hash });
    };
    await add("boot_sector", image.subarray(0, 512));
    const cluster = manifest.geometry.cluster_size;
    const recordSize = manifest.geometry.file_record_size;
    const mft = new Uint8Array(Number(manifest.mft.data_size));
    for (const extent of manifest.mft.extents) {
      if (extent.lcn === null) throw new Error("sparse MFT in manifest");
      const offset = Number(extent.vcn) * cluster;
      const start = Number(extent.lcn) * cluster;
      const length = Math.min(Number(extent.len) * cluster, mft.length - offset);
      if (offset > mft.length || start + length > image.length) {
        throw new Error("MFT outside image");
      }
      mft.set(image.subarray(start, start + length), offset);
    }
    const special = manifest.entries.filter((entry) =>
      entry.reparse_tag !== null ||
      entry.has_attribute_list || entry.nlink > 1 ||
      entry.streams.some((stream) => stream.compressed || stream.sparse || stream.name.length > 0)
    );
    const records = new Set([
      0,
      5,
      6,
      ...special.map((entry) => Number(entry.record)),
      ...manifest.fill.flatMap((fill) => [Number(fill.record_min), Number(fill.record_max)]),
      ...manifest.entries.map((entry) => Number(entry.record)),
      ...Array.from({ length: Math.min(256, mft.length / recordSize) }, (_, index) => index),
    ]);
    for (const number of records) {
      const raw = mft.subarray(number * recordSize, (number + 1) * recordSize);
      if (raw.length !== recordSize) throw new Error("record outside MFT");
      await add("file_record", raw);
      const fixed = fixedRecord(raw);
      if (fixed === null) continue;
      const header = view(fixed);
      await add(
        "attributes",
        fixed.subarray(header.getUint16(20, true), header.getUint32(24, true)),
      );
      for (const attribute of attributes(fixed)) {
        const data = view(attribute);
        const type = data.getUint32(0, true);
        if (attribute[8] === 1) {
          await add(
            "runlist",
            concatenate(attribute.subarray(16, 32), attribute.subarray(data.getUint16(32, true))),
          );
        }
        const selector = new Map([
          [0x10, selectors.STANDARD_INFORMATION],
          [0x30, selectors.FILE_NAME],
          [0xc0, selectors.REPARSE_POINT],
          [0x20, selectors.ATTRIBUTE_LIST],
          [0xb0, selectors.BITMAP],
        ]).get(type);
        if (selector !== undefined) {
          const bytes = value(attribute, image, cluster);
          if (bytes !== null) {
            let prefix = new Uint8Array([selector]);
            if (selector === selectors.BITMAP) {
              prefix = new Uint8Array(9);
              prefix[0] = selector;
              view(prefix).setBigUint64(1, BigInt(bytes.length) * 8n, true);
            }
            await add("typed_values", concatenate(prefix, bytes));
            if (selector === selectors.FILE_NAME && bytes.length >= 66) {
              await add(
                "typed_values",
                concatenate(
                  new Uint8Array([selectors.UTF16]),
                  bytes.subarray(66, 66 + bytes[64] * 2),
                ),
              );
            }
          }
        } else if (type === 0x80 || type === 0xa0) {
          await add(
            "typed_values",
            concatenate(
              new Uint8Array([type === 0x80 ? selectors.DATA : selectors.INDEX_ALLOCATION]),
              attribute,
              new Uint8Array([255, 255, 255, 255]),
            ),
          );
        }
      }
    }
  }
  for (const target of targets) {
    const path = `${destination}/${target}`;
    try {
      await Deno.remove(path, { recursive: true });
    } catch (error) {
      if (!(error instanceof Deno.errors.NotFound)) throw error;
    }
    await Deno.mkdir(path, { recursive: true });
    let total = 0;
    let count = 0;
    for (let index = 0; images.some((seeds) => index < seeds.get(target)!.length); index++) {
      for (const seeds of images) {
        const seed = seeds.get(target)![index];
        if (!seed || total + seed.bytes.length > budget) continue;
        await Deno.writeFile(`${path}/${seed.hash}`, seed.bytes);
        total += seed.bytes.length;
        count++;
      }
    }
    if (count === 0) throw new Error(`no seeds for ${target}`);
    console.log(`${target}: ${count} seeds, ${total}/${budget} bytes`);
  }
}

if (import.meta.main) await main();
