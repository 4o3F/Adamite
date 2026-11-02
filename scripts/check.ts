import { parse } from "@std/toml";

type Step = {
  name: string;
  command: "cargo" | "deno";
  args: string[];
  cwd?: string;
  env?: Record<string, string>;
};

async function run(step: Step): Promise<void> {
  console.log(`\nChecking ${step.name}`);
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

  const fuzzManifest = "crates/adamite-ntfs/fuzz/Cargo.toml";
  const hasFuzz = await exists(fuzzManifest);
  const steps: Step[] = [
    { name: "Rust formatting", command: "cargo", args: ["fmt", "--all", "--check"] },
  ];
  if (hasFuzz) {
    steps.push({
      name: "fuzz formatting",
      command: "cargo",
      args: ["fmt", "--manifest-path", fuzzManifest, "--check"],
    });
  } else {
    console.log(`Skipping fuzz steps: ${fuzzManifest} does not exist`);
  }

  for (const feature of ["--no-default-features", "--all-features"]) {
    steps.push(
      {
        name: `Clippy ${feature}`,
        command: "cargo",
        args: [
          "clippy",
          "--workspace",
          "--all-targets",
          "--locked",
          feature,
          "--",
          "-D",
          "warnings",
        ],
      },
      {
        name: `Rust tests ${feature}`,
        command: "cargo",
        args: ["test", "--workspace", "--locked", feature],
        env: { PROPTEST_CASES: "1024" },
      },
    );
  }
  steps.push({
    name: "Rust documentation",
    command: "cargo",
    args: ["doc", "--workspace", "--no-deps", "--all-features"],
    env: { RUSTDOCFLAGS: "-D warnings" },
  });
  for (const features of [[], ["--features", "alloc"]]) {
    steps.push({
      name: `no_std build ${features.length === 0 ? "without alloc" : "with alloc"}`,
      command: "cargo",
      args: ["build", "-p", "adamite-ntfs", "--target", "thumbv7em-none-eabihf", ...features],
    });
  }
  steps.push(
    { name: "dependency audit", command: "cargo", args: ["deny", "check"] },
    { name: "Deno formatting", command: "deno", args: ["fmt", "--check"] },
    { name: "Deno lint", command: "deno", args: ["lint"] },
    { name: "Deno type check", command: "deno", args: ["check", "scripts/"] },
    { name: "workflow syntax", command: "deno", args: ["task", "ci-lint"] },
  );
  if (Deno.args.includes("--fuzz") && hasFuzz) {
    steps.push({
      name: "install cargo-fuzz",
      command: "cargo",
      args: ["install", "cargo-fuzz", "--locked"],
    });
    steps.push({
      name: "fuzz build",
      command: "cargo",
      cwd: "crates/adamite-ntfs",
      args: ["+nightly", "fuzz", "build"],
    });
    const targets = ["boot_sector", "file_record", "attributes", "runlist", "typed_values"];
    for (const target of targets) {
      await requireCorpus(`crates/adamite-ntfs/fuzz/corpus/${target}`);
    }
    for (const target of targets) {
      steps.push({
        name: `fuzz ${target}`,
        command: "cargo",
        cwd: "crates/adamite-ntfs",
        args: [
          "+nightly",
          "fuzz",
          "run",
          target,
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
