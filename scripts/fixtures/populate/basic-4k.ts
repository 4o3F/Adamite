import { addFileFlags, command, pattern, type Population, writeAll } from "../lib.ts";

export async function populate(root: string): Promise<Population> {
  const write = (path: string, size: number) => Deno.writeFile(`${root}/${path}`, pattern(size));
  await write("empty.txt", 0);
  await write("tiny.txt", 100);
  await write("medium.bin", 64 * 1024);
  await Deno.mkdir(`${root}/bigdir`);
  for (let index = 0; index < 200; index++) {
    await write(`bigdir/f${index.toString().padStart(3, "0")}`, 0);
  }
  await Deno.mkdir(`${root}/hl`);
  await Deno.mkdir(`${root}/other`);
  await write("hl/orig.txt", 100);
  await Deno.link(`${root}/hl/orig.txt`, `${root}/hl/link-same-dir.txt`);
  await Deno.link(`${root}/hl/orig.txt`, `${root}/other/link-other-dir.txt`);
  await write("ads.txt", 0);
  await write("ads.txt:meta", 32);
  await write("ads.txt:big", 20 * 1024);
  for (const name of ["日本語.txt", "emoji-😀.txt", `${"n".repeat(251)}.txt`]) await write(name, 1);
  let deep = root;
  for (let index = 0; index < 40; index++) {
    deep += `/d${index.toString().padStart(2, "0")}`;
    await Deno.mkdir(deep);
  }
  await Deno.writeFile(`${deep}/leaf.txt`, pattern(1));
  const sparse = await Deno.open(`${root}/sparse.bin`, { createNew: true, write: true });
  try {
    await sparse.truncate(1024 * 1024);
    await writeAll(sparse, pattern(4096));
    await sparse.seek(512 * 1024, Deno.SeekMode.Start);
    await writeAll(sparse, pattern(4096));
  } finally {
    sparse.close();
  }
  await Deno.mkdir(`${root}/compressed`);
  await addFileFlags(`${root}/compressed`, 0x800);
  const content = new TextEncoder().encode("Adamite fixture text\n".repeat(14000)).subarray(
    0,
    256 * 1024,
  );
  await Deno.writeFile(`${root}/compressed/c.txt`, content);
  await write("reparse.bin", 0);
  const target = "tiny.txt";
  const encoded = Uint8Array.from(Array.from(target).flatMap((char) => [char.charCodeAt(0), 0]));
  const reparse = new Uint8Array(20 + encoded.length * 2);
  const view = new DataView(reparse.buffer);
  view.setUint32(0, 0xa000000c, true);
  view.setUint16(4, reparse.length - 8, true);
  view.setUint16(10, encoded.length, true);
  view.setUint16(12, encoded.length, true);
  view.setUint16(14, encoded.length, true);
  view.setUint32(16, 1, true);
  reparse.set(encoded, 20);
  reparse.set(encoded, 20 + encoded.length);
  await write("LongFileName.txt", 1);
  await command("setfattr", [
    "-n",
    "system.ntfs_dos_name",
    "-v",
    "LONGFI~1.TXT",
    `${root}/LongFileName.txt`,
  ]);
  await write("hidden.txt", 1);
  await addFileFlags(`${root}/hidden.txt`, 0x6);
  await write("deleted.txt", 1);
  const deleted = await Deno.lstat(`${root}/deleted.txt`);
  if (deleted.ino === null) throw new Error("deleted record has no inode number");
  await Deno.remove(`${root}/deleted.txt`);
  return {
    deleted: [{ path: "/deleted.txt", record: deleted.ino.toString() }],
    reparsePoints: [{ path: "/reparse.bin", data: reparse }],
  };
}
