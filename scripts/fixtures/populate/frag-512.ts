import { pattern, type Population, writeAll } from "../lib.ts";

function storageFull(error: unknown): boolean {
  return error instanceof Error && /space|storage.*full|ENOSPC/i.test(error.message);
}

async function writePattern(file: Deno.FsFile, size: number) {
  const bytes = pattern(size);
  for (let offset = 0; offset < bytes.length; offset += 512) {
    await writeAll(file, bytes.subarray(offset, offset + 512));
  }
}

export async function populate(root: string, attempt: number): Promise<Population> {
  await Deno.mkdir(`${root}/fill`);
  const files: string[] = [];
  const fragmentClusters = 600 + (attempt - 1) * 200;
  const reservationPath = `${root}/allocation-reserve.tmp`;
  const reservation = await Deno.open(reservationPath, { createNew: true, write: true });
  try {
    for (let index = 0; index < 65536; index++) {
      // A 255-unit name forces a 512-byte DATA value out of the 1 KiB record.
      const name = `f${index.toString().padStart(6, "0")}-${"x".repeat(247)}`;
      const path = `${root}/fill/${name}`;
      try {
        await Deno.writeFile(path, pattern(512));
        files.push(path);
      } catch (error) {
        if (!storageFull(error)) throw error;
        try {
          await Deno.remove(path);
        } catch (cleanup) {
          if (!(cleanup instanceof Deno.errors.NotFound)) throw cleanup;
        }
        break;
      }
    }
    if (files.length < 1200) throw new Error(`only ${files.length} one-cluster files fit`);
    for (let index = 0; index < files.length; index += 2) await Deno.remove(files[index]);
    const reserveClusters = Math.ceil(files.length / 2) - fragmentClusters - 1024;
    if (reserveClusters <= 0) {
      throw new FragmentationError("not enough freed clusters for fragmentation and growth");
    }
    // ntfs-3g consumes larger free extents first, including freed directory blocks.
    await writePattern(reservation, reserveClusters * 512);
  } finally {
    reservation.close();
  }
  const fragmented = await Deno.open(`${root}/frag.bin`, { createNew: true, write: true });
  try {
    // Convert DATA with a small runlist before growing it into attribute-list segments.
    await writePattern(fragmented, fragmentClusters * 512);
  } finally {
    fragmented.close();
  }
  await Deno.remove(reservationPath);
  await Deno.mkdir(`${root}/growth`);
  for (let index = 0; index < 1500; index++) {
    await Deno.writeFile(`${root}/growth/f${index.toString().padStart(4, "0")}`, pattern(1));
  }
  return { deleted: [] };
}

export class FragmentationError extends Error {}

export function checkFragmentation(mft: string, fragmented: string) {
  const data = mft.split(/(?=^Dumping attribute )/m).filter((block) =>
    /^Dumping attribute \$?DATA \(0x80\)/.test(block) && !/Attribute name:/.test(block)
  );
  const extents = data.reduce(
    (count, block) =>
      count + Array.from(block.matchAll(/^\s+0x[\da-f]+\s+0x[\da-f]+\s+0x[\da-f]+\s*$/gim)).length,
    0,
  );
  if (extents < 2) {
    throw new FragmentationError(`fragmentation postcondition: MFT has ${extents} physical runs`);
  }
  if (!/Dumping attribute \$?ATTRIBUTE_LIST \(0x20\)/.test(fragmented)) {
    throw new FragmentationError("fragmentation postcondition: frag.bin lacks ATTRIBUTE_LIST");
  }
  if (!/(?:Lowest|Starting) VCN:?\s+[1-9]\d*/.test(fragmented)) {
    throw new FragmentationError(
      "fragmentation postcondition: frag.bin lacks a continuation segment",
    );
  }
}
