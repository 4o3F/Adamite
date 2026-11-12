import { UntarStream } from "@std/tar";
import {
  artifactName,
  checkReleaseTag,
  main,
  type PackageArtifact,
  packageRelease,
  readVersion,
  releaseNotes,
  writeReleaseNotes,
} from "./package.ts";

function assert(condition: unknown, message: string): asserts condition {
  if (!condition) throw new Error(message);
}

function equal(actual: unknown, expected: unknown, message: string): void {
  assert(
    JSON.stringify(actual) === JSON.stringify(expected),
    `${message}: expected ${JSON.stringify(expected)}, received ${JSON.stringify(actual)}`,
  );
}

async function rejects(action: () => Promise<unknown>, message: string): Promise<void> {
  let caught: unknown;
  try {
    await action();
  } catch (error) {
    caught = error;
  }
  assert(caught instanceof Error && caught.message.includes(message), `expected error: ${message}`);
}

const version = "1.2.3-rc.1";

async function withTestTree(action: (root: string) => Promise<void>): Promise<void> {
  const directory = "target/package-test";
  await Deno.mkdir(directory, { recursive: true });
  const root = await Deno.makeTempDir({ dir: directory });
  try {
    await Deno.mkdir(`${root}/crates/adamite-cli`, { recursive: true });
    await Deno.writeTextFile(
      `${root}/crates/adamite-cli/Cargo.toml`,
      `[package]\nname = "adamite-cli"\nversion = "${version}"\n`,
    );
    await Deno.mkdir(`${root}/target/release`, { recursive: true });
    await action(root);
  } finally {
    await Deno.remove(root, { recursive: true });
  }
}

async function assertChecksum(artifact: PackageArtifact): Promise<void> {
  const bytes = await Deno.readFile(artifact.path);
  const digest = new Uint8Array(await crypto.subtle.digest("SHA-256", bytes));
  const hex = Array.from(digest, (byte) => byte.toString(16).padStart(2, "0")).join("");
  equal(artifact.checksumPath, `${artifact.path}.sha256`, "checksum filename");
  equal(
    await Deno.readTextFile(artifact.checksumPath),
    `${hex}  ${artifact.name}\n`,
    "checksum value and conventional file format",
  );
}

Deno.test("artifact names include the manifest version and target triple", () => {
  equal(
    artifactName(version, "Linux"),
    "adamite-1.2.3-rc.1-x86_64-unknown-linux-gnu.tar.gz",
    "Linux artifact",
  );
  equal(
    artifactName(version, "Windows"),
    "adamite-1.2.3-rc.1-x86_64-pc-windows-msvc.exe",
    "Windows artifact",
  );
});

Deno.test("Windows packaging copies the exe and writes its SHA-256 checksum", async () => {
  await withTestTree(async (root) => {
    await Deno.writeTextFile(`${root}/target/release/adamite.exe`, "abc");
    await Deno.writeTextFile(`${root}/target/release/adamite`, "wrong platform");
    const artifact = await packageRelease("Windows", root);
    equal(artifact.name, `adamite-${version}-x86_64-pc-windows-msvc.exe`, "artifact name");
    equal(artifact.path, `${root}/dist/${artifact.name}`, "artifact directory");
    equal(await Deno.readTextFile(artifact.path), "abc", "copied binary");
    await assertChecksum(artifact);
    equal(
      await Deno.readTextFile(artifact.checksumPath),
      `ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad  ${artifact.name}\n`,
      "known SHA-256 value",
    );
  });
});

Deno.test("Linux packaging archives exactly adamite with mode 755 and its binary content", async () => {
  await withTestTree(async (root) => {
    const binary = Uint8Array.from({ length: 1537 }, (_, index) => index % 256);
    await Deno.writeFile(`${root}/target/release/adamite`, binary, { mode: 0o600 });
    await Deno.writeTextFile(`${root}/target/release/adamite.exe`, "wrong platform");
    const artifact = await packageRelease("Linux", root);
    equal(artifact.name, `adamite-${version}-x86_64-unknown-linux-gnu.tar.gz`, "artifact name");
    equal(artifact.path, `${root}/dist/${artifact.name}`, "artifact directory");
    const archive = await Deno.readFile(artifact.path);
    const entries = new Blob([archive]).stream()
      .pipeThrough(new DecompressionStream("gzip"))
      .pipeThrough(new UntarStream());
    let count = 0;
    for await (const entry of entries) {
      count++;
      equal(entry.path, "adamite", "tar entry name");
      equal(entry.header.mode, 0o755, "tar entry mode");
      equal(entry.header.typeflag, "0", "tar entry is a regular file");
      equal(entry.header.size, binary.length, "tar entry size");
      assert(entry.readable, "tar entry must contain a file");
      const content = new Uint8Array(await new Response(entry.readable).arrayBuffer());
      equal(Array.from(content), Array.from(binary), "tar entry content");
    }
    equal(count, 1, "tar entry count");
    await assertChecksum(artifact);
  });
});

Deno.test("unknown OS fails before reading the manifest or writing output", async () => {
  await withTestTree(async (root) => {
    await Deno.remove(`${root}/crates`, { recursive: true });
    for (const os of ["Darwin", "linux", "", "FreeBSD"]) {
      await rejects(() => packageRelease(os, root), `unknown OS: ${os}; expected Linux or Windows`);
    }
    const names = (await Array.fromAsync(Deno.readDir(root))).map((entry) => entry.name);
    assert(!names.includes("dist"), "unknown OS must not create dist");
  });
});

Deno.test("each platform requires its own release binary and reports the build command", async () => {
  await withTestTree(async (root) => {
    for (const os of ["Linux", "Windows"] as const) {
      const name = os === "Windows" ? "adamite.exe" : "adamite";
      const other = os === "Windows" ? "adamite" : "adamite.exe";
      await Deno.writeTextFile(`${root}/target/release/${other}`, "wrong platform");
      await rejects(
        () => packageRelease(os, root),
        `missing release binary: ${root}/target/release/${name}; run cargo build --release -p adamite-cli --locked`,
      );
      await Deno.remove(`${root}/target/release/${other}`);
    }
    const names = (await Array.fromAsync(Deno.readDir(root))).map((entry) => entry.name);
    assert(!names.includes("dist"), "missing binary must not create dist");
  });
});

Deno.test("release tags must exactly match v plus the CLI manifest version", async () => {
  await withTestTree(async (root) => {
    equal(await readVersion(root), version, "manifest version");
    await checkReleaseTag(`v${version}`, root);
    for (const tag of [version, "v1.2.3", `v${version}\n`, "v9.0.0", ""]) {
      await rejects(
        () => checkReleaseTag(tag, root),
        `release tag mismatch: expected v${version}, received ${tag}`,
      );
    }
  });
});

Deno.test("manifest must declare the CLI package and a nonempty string version", async () => {
  await withTestTree(async (root) => {
    for (
      const contents of [
        "",
        '[package]\nname = "other"\nversion = "1.0.0"',
        '[package]\nname = "adamite-cli"',
        '[package]\nname = "adamite-cli"\nversion = 1',
        '[package]\nname = "adamite-cli"\nversion = ""',
      ]
    ) {
      await Deno.writeTextFile(`${root}/crates/adamite-cli/Cargo.toml`, contents);
      await rejects(
        () => readVersion(root),
        "expected an adamite-cli package with a string version",
      );
    }
  });
});

Deno.test("command dispatch validates arguments and routes release checks and packaging", async () => {
  await withTestTree(async (root) => {
    for (
      const args of [[], ["--release-check"], ["Linux", "extra"], [
        "--release-check",
        "v1",
        "extra",
      ]]
    ) {
      await rejects(() => main(args, root), "usage: deno task package <Linux|Windows>");
    }
    await main(["--release-check", `v${version}`], root);
    await rejects(() => main(["--release-check", "wrong"], root), "release tag mismatch");
    await rejects(() => main(["Darwin"], root), "unknown OS: Darwin");
    await rejects(() => main(["Linux"], root), "missing release binary");
    await Deno.writeTextFile(`${root}/target/release/adamite.exe`, "abc");
    await main(["Windows"], root);
    equal(
      await Deno.readTextFile(`${root}/dist/adamite-${version}-x86_64-pc-windows-msvc.exe`),
      "abc",
      "command packages the binary",
    );
  });
});

Deno.test("release notes take exactly the version's changelog section", () => {
  const changelog = "# Changelog\n\n## 1.2.3-rc.1\n\n- Added a thing.\n\n## 1.2.2\n\n- Older.\n";
  equal(releaseNotes(changelog, "1.2.3-rc.1"), "- Added a thing.\n", "section body only");
  equal(releaseNotes(changelog, "1.2.2"), "- Older.\n", "last section runs to the end");
  let caught: unknown;
  try {
    releaseNotes(changelog, "1.2.3");
  } catch (error) {
    caught = error;
  }
  assert(caught instanceof Error && caught.message.includes('no "## 1.2.3" section'), "missing");
  try {
    releaseNotes("## 1.2.3-rc.1\n\n## 1.2.2\n- Older.\n", "1.2.3-rc.1");
    caught = undefined;
  } catch (error) {
    caught = error;
  }
  assert(caught instanceof Error && caught.message.includes("is empty"), "empty section");
});

Deno.test("release-notes checks the tag and writes target/RELEASE_NOTES.md", async () => {
  await withTestTree(async (root) => {
    await Deno.writeTextFile(`${root}/CHANGELOG.md`, `## ${version}\n\n- Notes.\n`);
    await rejects(() => writeReleaseNotes("v0.0.1", root), "release tag mismatch");
    await main(["--release-notes", `v${version}`], root);
    equal(
      await Deno.readTextFile(`${root}/target/RELEASE_NOTES.md`),
      "- Notes.\n",
      "notes file",
    );
  });
});
