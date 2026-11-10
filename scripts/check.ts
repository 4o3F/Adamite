import { parse } from "@std/toml";

type Step = {
  name: string;
  command: "cargo" | "deno";
  args: string[];
  cwd?: string;
  env?: Record<string, string>;
  scratchCorpus?: string;
};

type FuzzPackage = {
  name: string;
  targets: readonly string[];
  tests: readonly string[];
};

export const fuzzPackages: readonly FuzzPackage[] = [
  {
    name: "adamite-ntfs",
    targets: ["boot_sector", "file_record", "attributes", "runlist", "typed_values"],
    tests: [],
  },
  { name: "adamite-core", targets: ["scan"], tests: ["harness"] },
];

async function run(step: Step): Promise<void> {
  console.log(`\nChecking ${step.name}`);
  if (step.scratchCorpus) await resetScratchCorpus(step.scratchCorpus);
  const status = await new Deno.Command(step.command, {
    args: step.args,
    cwd: step.cwd,
    env: step.env,
    stdin: "inherit",
    stdout: "inherit",
    stderr: "inherit",
  }).spawn().status;
  if (!status.success) {
    throw new Error(`${step.name} failed (exit ${status.code})`);
  }
}

export async function resetScratchCorpus(path: string): Promise<void> {
  try {
    await Deno.remove(path, { recursive: true });
  } catch (error) {
    if (!(error instanceof Deno.errors.NotFound)) throw error;
  }
  await Deno.mkdir(path, { recursive: true });
}

async function exists(path: string): Promise<boolean> {
  try {
    await Deno.stat(path);
    return true;
  } catch (error) {
    if (error instanceof Deno.errors.NotFound) return false;
    throw error;
  }
}

export async function requireCorpus(path: string): Promise<void> {
  try {
    for await (const entry of Deno.readDir(path)) {
      if (entry.isFile && (await Deno.stat(`${path}/${entry.name}`)).size > 0) return;
    }
  } catch (error) {
    if (!(error instanceof Deno.errors.NotFound)) throw error;
    throw new Error(`missing fuzz corpus: ${path}; run deno task fuzz-corpus`);
  }
  throw new Error(`empty fuzz corpus: ${path}; run deno task fuzz-corpus`);
}

export async function requireFuzzCorpora(packages: readonly FuzzPackage[]): Promise<void> {
  for (const pkg of packages) {
    for (const target of pkg.targets) {
      await requireCorpus(`crates/${pkg.name}/fuzz/corpus/${target}`);
    }
  }
}

export function checkSteps(options: { packages: readonly FuzzPackage[]; fuzz: boolean }): Step[] {
  const steps: Step[] = [
    { name: "Rust formatting", command: "cargo", args: ["fmt", "--all", "--check"] },
  ];
  for (const pkg of options.packages) {
    const manifest = `crates/${pkg.name}/fuzz/Cargo.toml`;
    steps.push(
      {
        name: `fuzz formatting ${pkg.name}`,
        command: "cargo",
        args: ["fmt", "--manifest-path", manifest, "--check"],
      },
      {
        name: `fuzz Clippy ${pkg.name}`,
        command: "cargo",
        args: [
          "clippy",
          "--manifest-path",
          manifest,
          "--all-targets",
          "--locked",
          "--",
          "-D",
          "warnings",
        ],
        // The parser exposes its attribute-region constructor only in fuzz builds.
        env: { RUSTFLAGS: "--cfg fuzzing" },
      },
    );
    for (const target of pkg.tests) {
      steps.push({
        name: `fuzz tests ${pkg.name} ${target}`,
        command: "cargo",
        args: ["test", "--manifest-path", manifest, "--test", target, "--locked"],
      });
    }
  }

  steps.push(
    {
      name: "Clippy",
      command: "cargo",
      args: ["clippy", "--workspace", "--all-targets", "--locked", "--", "-D", "warnings"],
    },
    {
      name: "Rust tests",
      command: "cargo",
      args: ["test", "--workspace", "--locked"],
      env: { PROPTEST_CASES: "1024" },
    },
  );
  steps.push({
    name: "Rust documentation",
    command: "cargo",
    args: ["doc", "--workspace", "--no-deps"],
    env: { RUSTDOCFLAGS: "-D warnings" },
  });
  steps.push({
    name: "no_std build",
    command: "cargo",
    args: ["build", "-p", "adamite-ntfs", "--target", "thumbv7em-none-eabihf"],
  });
  steps.push(
    { name: "dependency audit", command: "cargo", args: ["deny", "check"] },
    { name: "Deno formatting", command: "deno", args: ["fmt", "--check"] },
    { name: "Deno lint", command: "deno", args: ["lint"] },
    { name: "Deno type check", command: "deno", args: ["check", "scripts/"] },
    {
      name: "Deno fixture tests",
      command: "deno",
      args: ["test", "--frozen", "--allow-read=scripts/fixtures/testdata", "scripts/fixtures/"],
    },
    {
      name: "check script tests",
      command: "deno",
      args: [
        "test",
        "--frozen",
        "--allow-read=deno.json,.github/workflows/ci.yml,crates/adamite-ntfs/fuzz/Cargo.toml,crates/adamite-core/fuzz/Cargo.toml",
        "scripts/check_test.ts",
      ],
    },
    { name: "workflow syntax", command: "deno", args: ["task", "ci-lint"] },
  );
  if (options.fuzz && options.packages.length > 0) {
    steps.push({
      name: "install cargo-fuzz",
      command: "cargo",
      args: ["install", "cargo-fuzz", "--locked"],
    });
    for (const pkg of options.packages) {
      steps.push({
        name: `fuzz build ${pkg.name}`,
        command: "cargo",
        cwd: `crates/${pkg.name}`,
        args: ["+nightly", "fuzz", "build"],
      });
      for (const target of pkg.targets) {
        const scratchCorpus = `target/fuzz-smoke/${pkg.name}/${target}`;
        steps.push({
          name: `fuzz ${pkg.name} ${target}`,
          command: "cargo",
          cwd: `crates/${pkg.name}`,
          scratchCorpus,
          args: [
            "+nightly",
            "fuzz",
            "run",
            target,
            // libFuzzer writes new inputs only to its first corpus directory.
            `../../${scratchCorpus}/`,
            `fuzz/corpus/${target}/`,
            "--",
            "-max_total_time=30",
            "-max_len=262144",
            "-timeout=2",
            "-rss_limit_mb=2048",
          ],
        });
      }
    }
  }
  return steps;
}

async function main(): Promise<void> {
  if (Deno.args.some((arg) => arg !== "--fuzz") || Deno.args.length > 1) {
    throw new Error("usage: deno task check [--fuzz]");
  }

  const toolchain = parse(await Deno.readTextFile("rust-toolchain.toml"));
  const manifest = parse(await Deno.readTextFile("Cargo.toml"));
  const channel = (toolchain.toolchain as Record<string, unknown>)?.channel;
  const workspacePackage = (manifest.workspace as Record<string, unknown>)?.package;
  const rustVersion = (workspacePackage as Record<string, unknown>)?.["rust-version"];
  if (typeof channel !== "string" || channel !== rustVersion) {
    throw new Error("rust-toolchain.toml channel and workspace rust-version must agree");
  }
  console.log(`Toolchain and rust-version agree: ${channel}`);

  const packages: FuzzPackage[] = [];
  for (const pkg of fuzzPackages) {
    const path = `crates/${pkg.name}/fuzz/Cargo.toml`;
    if (await exists(path)) packages.push(pkg);
    else console.log(`Skipping fuzz steps: ${path} does not exist`);
  }
  const fuzz = Deno.args.includes("--fuzz");
  if (fuzz) await requireFuzzCorpora(packages);
  const steps = checkSteps({ packages, fuzz });
  for (const step of steps) await run(step);
  console.log("\nAll checks passed");
}

if (import.meta.main) {
  try {
    await main();
  } catch (error) {
    console.error(error instanceof Error ? error.message : error);
    Deno.exit(1);
  }
}
