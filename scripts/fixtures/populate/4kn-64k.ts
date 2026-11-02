import { pattern, type Population } from "../lib.ts";

export async function populate(root: string): Promise<Population> {
  await Deno.writeFile(`${root}/a.txt`, pattern(100));
  await Deno.writeFile(`${root}/b.bin`, pattern(200 * 1024));
  return { deleted: [] };
}
