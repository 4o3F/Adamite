import { compactManifest, json, type ManifestInput } from "./manifest.ts";

if (import.meta.main) {
  for (const name of ["basic-4k", "frag-512", "4kn-64k"]) {
    const path = `tests/fixtures/ntfs/${name}.json`;
    const input: ManifestInput = JSON.parse(await Deno.readTextFile(path));
    const output = json(compactManifest(input));
    const size = new TextEncoder().encode(output).length;
    if (size > 256 * 1024) throw new Error(`${path}: ${size} exceeds 256 KiB`);
    await Deno.writeTextFile(path, output);
    console.log(`${path}: ${size} bytes`);
  }
}
