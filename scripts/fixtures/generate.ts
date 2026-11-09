import {
  command,
  setAttribute,
  setFixtureTimes,
  text,
  withMountedImage,
  withTemporaryDirectory,
} from "./lib.ts";
import { blocks, collect, finalize, json, type Manifest } from "./manifest.ts";
import { populate as basic } from "./populate/basic-4k.ts";
import {
  checkFragmentation,
  FragmentationError,
  populate as fragmented,
} from "./populate/frag-512.ts";
import { populate as largeSector } from "./populate/4kn-64k.ts";

const profiles = {
  "basic-4k": { size: 16 * 1024 * 1024, sector: 512, cluster: 4096, record: 1024 },
  "frag-512": { size: 32 * 1024 * 1024, sector: 512, cluster: 512, record: 1024 },
  "4kn-64k": { size: 64 * 1024 * 1024, sector: 4096, cluster: 65536, record: 4096 },
} as const;
type ImageName = keyof typeof profiles;
const output = "tests/fixtures/ntfs";
const budget = 1024 * 1024;

function requireCondition(condition: boolean, message: string) {
  if (!condition) throw new Error(`fixture postcondition: ${message}`);
}

function validate(manifest: Manifest, name: ImageName) {
  const expected = profiles[name];
  requireCondition(
    manifest.geometry.bytes_per_sector === expected.sector &&
      manifest.geometry.cluster_size === expected.cluster &&
      manifest.geometry.file_record_size === expected.record,
    `${name} geometry differs from its profile`,
  );
  const entry = (path: string) => {
    const value = manifest.entries.find((item) => item.path === path);
    if (!value) throw new Error(`missing fixture entry ${path}`);
    return value;
  };
  const stream = (path: string, streamName = "") => {
    const value = entry(path).streams.find((item) =>
      item.attribute_type === 0x80 && String.fromCharCode(...item.name) === streamName
    );
    if (!value) throw new Error(`missing stream ${path}:${streamName}`);
    return value;
  };
  if (name === "basic-4k") {
    for (const item of manifest.entries) {
      // The final reparse xattr write may change its mtime; finalize reads it offline.
      if (item.path !== "/reparse.bin") {
        requireCondition(item.mtime_filetime === "132223104000000000", `${item.path} mtime`);
      }
    }
    requireCondition(
      stream("/empty.txt").size === "0" && stream("/tiny.txt").resident,
      "empty/tiny resident values",
    );
    requireCondition(
      stream("/tiny.txt").size === "100" && stream("/medium.bin").size === "65536" &&
        !stream("/medium.bin").resident,
      "basic file sizes/storage",
    );
    requireCondition(
      stream("/ads.txt", "meta").size === "32" && stream("/ads.txt", "meta").resident,
      "resident ADS",
    );
    requireCondition(
      stream("/ads.txt", "big").size === "20480" && !stream("/ads.txt", "big").resident,
      "non-resident ADS",
    );
    for (const path of ["/hl/orig.txt", "/hl/link-same-dir.txt", "/other/link-other-dir.txt"]) {
      requireCondition(
        entry(path).nlink === 3 && entry(path).record === entry("/hl/orig.txt").record,
        "three hard links",
      );
    }
    requireCondition(
      new Set(entry("/hl/orig.txt").names.map((item) => item.parent_record)).size === 2,
      "hard-link parents",
    );
    requireCondition(
      manifest.entries.filter((item) => /^\/bigdir\/f\d{3}$/.test(item.path)).length === 200,
      "bigdir file count",
    );
    for (const path of ["/日本語.txt", "/emoji-😀.txt", `/${"n".repeat(251)}.txt`]) entry(path);
    entry(
      "/" + Array.from({ length: 40 }, (_, index) =>
        `d${index.toString().padStart(2, "0")}`).join("/") +
        "/leaf.txt",
    );
    requireCondition(
      stream("/sparse.bin").size === "1048576" && stream("/sparse.bin").sparse,
      "sparse stream",
    );
    requireCondition(
      stream("/compressed/c.txt").size === "262144" && stream("/compressed/c.txt").compressed,
      "compressed stream",
    );
    requireCondition(entry("/reparse.bin").reparse_tag === 0xa000000c, "symlink reparse tag");
    requireCondition(
      entry("/LongFileName.txt").names.some((item) =>
        item.namespace === 2 && String.fromCharCode(...item.name) === "LONGFI~1.TXT"
      ),
      "DOS alias",
    );
    requireCondition((entry("/hidden.txt").attributes & 6) === 6, "hidden/system flags");
    requireCondition(manifest.deleted.length === 1, "deleted record metadata");
  } else if (name === "4kn-64k") {
    requireCondition(
      stream("/a.txt").size === "100" && stream("/b.bin").size === "204800",
      "4Kn file sizes",
    );
  } else {
    requireCondition(
      manifest.mft.extents.length >= 2 && entry("/frag.bin").has_attribute_list,
      "fragmented MFT and DATA list",
    );
  }
}

async function versions(): Promise<Record<string, string>> {
  const result: Record<string, string> = {};
  for (const tool of ["mkntfs", "ntfsinfo", "getfattr", "setfattr", "zstd", "mountpoint", "sudo"]) {
    const value = await command(tool, ["--version"]);
    result[tool] =
      new TextDecoder().decode(value.stdout.length ? value.stdout : value.stderr).trim().split(
        "\n",
      )[0];
  }
  await command("sudo", ["-v"]);
  const driver = await command("sudo", ["-n", "ntfs-3g", "--version"]);
  result["ntfs-3g"] =
    new TextDecoder().decode(driver.stdout.length ? driver.stdout : driver.stderr).trim().split(
      "\n",
    )[0];
  return result;
}

async function publish(directory: string, name: ImageName) {
  const compressed = `${directory}/${name}.img.zst`;
  const size = (await Deno.stat(compressed)).size;
  let total = size;
  for await (const entry of Deno.readDir(output)) {
    if (entry.isFile && entry.name.endsWith(".img.zst") && entry.name !== `${name}.img.zst`) {
      total += (await Deno.stat(`${output}/${entry.name}`)).size;
    }
  }
  requireCondition(total <= budget, `compressed budget ${total} > ${budget}`);
  for (const extension of ["img.zst", "json"]) {
    await Deno.copyFile(
      `${directory}/${name}.${extension}`,
      `${output}/.${name}.${extension}.pending`,
    );
  }
  for (const extension of ["img.zst", "json"]) {
    await Deno.rename(`${output}/.${name}.${extension}.pending`, `${output}/${name}.${extension}`);
  }
  console.log(`${name}: ${size} compressed bytes; all fixtures ${total}/${budget} bytes`);
}

async function generate(name: ImageName, generator: Record<string, string>, attempt: number) {
  const profile = profiles[name];
  await withTemporaryDirectory(async (directory) => {
    const image = `${directory}/${name}.img`;
    await Deno.writeFile(image, new Uint8Array());
    await Deno.truncate(image, profile.size);
    await command("mkntfs", [
      "-F",
      "-Q",
      "-q",
      "-T",
      "-s",
      String(profile.sector),
      "-c",
      String(profile.cluster),
      "-L",
      `ADAMITE_${name}`,
      image,
    ]);
    const collected = await withMountedImage(image, `${directory}/mount`, async () => {
      const population = name === "basic-4k"
        ? await basic(`${directory}/mount`)
        : name === "frag-512"
        ? await fragmented(`${directory}/mount`, attempt)
        : await largeSector(`${directory}/mount`);
      await setFixtureTimes(`${directory}/mount`);
      const entries = await collect(`${directory}/mount`);
      const reparsePoints = population.reparsePoints ?? [];
      // ntfs-3g can return EIO on lookup after this write. Do no further mount walks.
      for (const { path, data } of reparsePoints) {
        await setAttribute(`${directory}/mount${path}`, "system.ntfs_reparse_data", data);
      }
      return {
        entries,
        deleted: population.deleted,
        reparsePaths: reparsePoints.map(({ path }) => path),
      };
    });
    const volume = await text("ntfsinfo", ["-m", image]);
    const flags = volume.match(/^\s*Volume Flags:\s*0x([\da-f]+)/im);
    requireCondition(
      flags !== null && (parseInt(flags[1], 16) & 1) === 0,
      "volume is not clean after unmount",
    );
    if (name === "frag-512") {
      checkFragmentation(
        await text("ntfsinfo", ["-v", "-i", "0", image]),
        await text("ntfsinfo", ["-v", "-F", "/frag.bin", image]),
      );
    }
    if (name === "basic-4k") {
      requireCondition(
        blocks(await text("ntfsinfo", ["-v", "-F", "/bigdir", image]), 0xa0).length > 0,
        "bigdir lacks INDEX_ALLOCATION",
      );
    }
    const manifest = await finalize(
      image,
      name,
      BigInt(profile.size),
      generator,
      collected.entries,
      collected.deleted,
      collected.reparsePaths,
    );
    validate(manifest, name);
    const serialized = json(manifest);
    requireCondition(
      new TextEncoder().encode(serialized).length <= 256 * 1024,
      "manifest exceeds 256 KiB",
    );
    await Deno.writeTextFile(`${directory}/${name}.json`, serialized);
    await command("zstd", ["-19", "--check", "-f", image, "-o", `${directory}/${name}.img.zst`]);
    await publish(directory, name);
  });
}

async function main() {
  if (Deno.args.includes("--help") || Deno.args.includes("-h")) {
    console.log(
      "Usage: deno task fixtures [basic-4k|frag-512|4kn-64k]\nLinux; mkntfs, ntfs-3g, zstd, attr and sudo are required for generation.",
    );
    return;
  }
  if (Deno.build.os !== "linux") throw new Error("fixture generation requires Linux");
  if (Deno.args.length > 1 || (Deno.args.length === 1 && !(Deno.args[0] in profiles))) {
    throw new Error("usage: deno task fixtures [basic-4k|frag-512|4kn-64k]");
  }
  const selected = Deno.args.length === 0
    ? Object.keys(profiles) as ImageName[]
    : [Deno.args[0] as ImageName];
  const generator = await versions();
  await Deno.mkdir(output, { recursive: true });
  for (const name of selected) {
    const attempts = name === "frag-512" ? 3 : 1;
    for (let attempt = 1; attempt <= attempts; attempt++) {
      try {
        await generate(name, generator, attempt);
        break;
      } catch (error) {
        if (!(error instanceof FragmentationError) || attempt === attempts) throw error;
        console.warn(`${name} attempt ${attempt}/${attempts}: ${error}`);
      }
    }
  }
}

if (import.meta.main) await main();
