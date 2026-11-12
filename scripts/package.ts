import { TarStream, type TarStreamInput } from "@std/tar";
import { parse } from "@std/toml";

export type PackageOs = "Linux" | "Windows";
export type PackageArtifact = { name: string; path: string; checksumPath: string };

export function parseOs(value: string): PackageOs {
  if (value === "Linux" || value === "Windows") return value;
  throw new Error(`unknown OS: ${value}; expected Linux or Windows`);
}

export async function readVersion(root: string): Promise<string> {
  const path = `${root}/crates/adamite-cli/Cargo.toml`;
  const manifest = parse(await Deno.readTextFile(path));
  const pkg = manifest.package as Record<string, unknown> | undefined;
  if (pkg?.name !== "adamite-cli" || typeof pkg.version !== "string" || !pkg.version) {
    throw new Error(`expected an adamite-cli package with a string version in ${path}`);
  }
  return pkg.version;
}

export function artifactName(version: string, os: PackageOs): string {
  const suffix = os === "Windows"
    ? "x86_64-pc-windows-msvc.exe"
    : "x86_64-unknown-linux-gnu.tar.gz";
  return `adamite-${version}-${suffix}`;
}

async function linuxArchive(binary: Uint8Array): Promise<Uint8Array<ArrayBuffer>> {
  const stream = ReadableStream.from<TarStreamInput>([{
    type: "file",
    path: "adamite",
    size: binary.length,
    readable: new Blob([binary.slice()]).stream(),
    options: { mode: 0o755, mtime: 0 },
  }]).pipeThrough(new TarStream()).pipeThrough(new CompressionStream("gzip"));
  return new Uint8Array(await new Response(stream).arrayBuffer());
}

async function sha256(bytes: Uint8Array<ArrayBuffer>): Promise<string> {
  const digest = new Uint8Array(await crypto.subtle.digest("SHA-256", bytes));
  return Array.from(digest, (byte) => byte.toString(16).padStart(2, "0")).join("");
}

export async function packageRelease(value: string, root: string): Promise<PackageArtifact> {
  const os = parseOs(value);
  const version = await readVersion(root);
  const binaryPath = `${root}/target/release/adamite${os === "Windows" ? ".exe" : ""}`;
  let binary: Uint8Array<ArrayBuffer>;
  try {
    binary = await Deno.readFile(binaryPath);
  } catch (error) {
    if (!(error instanceof Deno.errors.NotFound)) throw error;
    throw new Error(
      `missing release binary: ${binaryPath}; run cargo build --release -p adamite-cli --locked`,
    );
  }
  const name = artifactName(version, os);
  const path = `${root}/dist/${name}`;
  const checksumPath = `${path}.sha256`;
  const bytes = os === "Linux" ? await linuxArchive(binary) : binary;
  const checksum = await sha256(bytes);
  await Deno.mkdir(`${root}/dist`, { recursive: true });
  await Deno.writeFile(path, bytes);
  await Deno.writeTextFile(checksumPath, `${checksum}  ${name}\n`);
  return { name, path, checksumPath };
}

export async function checkReleaseTag(tag: string, root: string): Promise<void> {
  const expected = `v${await readVersion(root)}`;
  if (tag !== expected) {
    throw new Error(`release tag mismatch: expected ${expected}, received ${tag}`);
  }
}

export function releaseNotes(changelog: string, version: string): string {
  const heading = `## ${version}`;
  const lines = changelog.split("\n");
  const start = lines.findIndex((line) => line.trimEnd() === heading);
  if (start < 0) throw new Error(`CHANGELOG.md has no "${heading}" section`);
  const rest = lines.slice(start + 1);
  const end = rest.findIndex((line) => line.startsWith("## "));
  const body = (end < 0 ? rest : rest.slice(0, end)).join("\n").trim();
  if (!body) throw new Error(`CHANGELOG.md section "${heading}" is empty`);
  return `${body}\n`;
}

export async function writeReleaseNotes(tag: string, root: string): Promise<string> {
  await checkReleaseTag(tag, root);
  const notes = releaseNotes(await Deno.readTextFile(`${root}/CHANGELOG.md`), tag.slice(1));
  await Deno.mkdir(`${root}/target`, { recursive: true });
  const path = `${root}/target/RELEASE_NOTES.md`;
  await Deno.writeTextFile(path, notes);
  return path;
}

export async function main(args: readonly string[], root: string): Promise<void> {
  if (args.length === 2 && args[0] === "--release-check") {
    await checkReleaseTag(args[1], root);
    console.log(`Release tag matches: ${args[1]}`);
  } else if (args.length === 2 && args[0] === "--release-notes") {
    console.log(`Wrote ${await writeReleaseNotes(args[1], root)}`);
  } else if (args.length === 1 && !args[0].startsWith("--release-")) {
    const artifact = await packageRelease(args[0], root);
    console.log(`Packaged ${artifact.path} and ${artifact.checksumPath}`);
  } else {
    throw new Error(
      "usage: deno task package <Linux|Windows>, deno task release-check <tag> or deno task release-notes <tag>",
    );
  }
}

if (import.meta.main) {
  try {
    await main(Deno.args, ".");
  } catch (error) {
    console.error(error instanceof Error ? error.message : error);
    Deno.exit(1);
  }
}
