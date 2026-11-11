import { parse as parseToml } from "@std/toml";
import { parse as parseYaml } from "@std/yaml";
import {
  checkSteps,
  fuzzPackages,
  requireCorpus,
  requireFuzzCorpora,
  resetScratchCorpus,
} from "./check.ts";

function assert(condition: unknown, message: string): asserts condition {
  if (!condition) throw new Error(message);
}

function equal(actual: unknown, expected: unknown, message: string): void {
  assert(
    JSON.stringify(actual) === JSON.stringify(expected),
    `${message}: expected ${JSON.stringify(expected)}, received ${JSON.stringify(actual)}`,
  );
}

Deno.test("fuzz corpus preflight rejects missing, empty and unusable directories", async () => {
  const original = { readDir: Deno.readDir, stat: Deno.stat };
  Deno.readDir = async function* (path) {
    if (path === "missing") throw new Deno.errors.NotFound();
    if (path === "empty") return;
    yield {
      name: "seed",
      isFile: path !== "directory",
      isDirectory: path === "directory",
      isSymlink: false,
    };
  };
  Deno.stat = () => Promise.resolve({ size: 0 } as Deno.FileInfo);
  try {
    for (const path of ["missing", "empty", "directory", "zero"]) {
      let rejected = false;
      try {
        await requireCorpus(path);
      } catch (error) {
        rejected = error instanceof Error &&
          error.message.includes(`${path}; run deno task fuzz-corpus`);
      }
      if (!rejected) throw new Error(`corpus ${path} must fail preflight`);
    }
  } finally {
    Object.assign(Deno, original);
  }
});

Deno.test("fuzz corpus preflight accepts a directory containing a seed", async () => {
  const original = { readDir: Deno.readDir, stat: Deno.stat };
  Deno.readDir = async function* () {
    yield { name: "seed", isFile: true, isDirectory: false, isSymlink: false };
  };
  Deno.stat = () => Promise.resolve({ size: 1 } as Deno.FileInfo);
  try {
    await requireCorpus("present");
  } finally {
    Object.assign(Deno, original);
  }
});

Deno.test("normal checks format and lint both excluded fuzz packages and test the core harness", () => {
  const steps = checkSteps({ packages: fuzzPackages, fuzz: false });
  for (const pkg of fuzzPackages) {
    const manifest = `crates/${pkg.name}/fuzz/Cargo.toml`;
    equal(
      steps.find((step) => step.name === `fuzz formatting ${pkg.name}`)?.args,
      ["fmt", "--manifest-path", manifest, "--check"],
      `${pkg.name} formatting`,
    );
    equal(
      steps.find((step) => step.name === `fuzz Clippy ${pkg.name}`)?.args,
      ["clippy", "--manifest-path", manifest, "--all-targets", "--locked", "--", "-D", "warnings"],
      `${pkg.name} Clippy`,
    );
    equal(
      steps.find((step) => step.name === `fuzz Clippy ${pkg.name}`)?.env,
      { RUSTFLAGS: "--cfg fuzzing" },
      `${pkg.name} fuzz-only parser API`,
    );
  }
  equal(
    steps.find((step) => step.name === "fuzz tests adamite-core harness")?.args,
    [
      "test",
      "--manifest-path",
      "crates/adamite-core/fuzz/Cargo.toml",
      "--test",
      "harness",
      "--locked",
    ],
    "core harness tests",
  );
});

Deno.test("normal checks include fixture and check-script tests with scoped read permissions", () => {
  const steps = checkSteps({ packages: fuzzPackages, fuzz: false });
  equal(
    steps.find((step) => step.name === "Deno fixture tests"),
    {
      name: "Deno fixture tests",
      command: "deno",
      args: ["test", "--frozen", "--allow-read=scripts/fixtures/testdata", "scripts/fixtures/"],
    },
    "fixture tests",
  );
  const script = steps.find((step) => step.name === "check script tests");
  assert(
    script?.command === "deno" && script.args.includes("scripts/check_test.ts"),
    "missing script tests",
  );
  equal(
    script.args.filter((arg) => arg.startsWith("--allow-")),
    ["--allow-read=deno.json,.github/workflows/ci.yml,crates/adamite-ntfs/fuzz/Cargo.toml,crates/adamite-core/fuzz/Cargo.toml"],
    "check-script permissions",
  );
});

Deno.test("normal checks run once without feature flags and keep the no_std build parser-only", () => {
  const steps = checkSteps({ packages: fuzzPackages, fuzz: false });
  equal(steps.filter((step) => step.args[0] === "clippy" && step.args.includes("--workspace")), [
    {
      name: "Clippy",
      command: "cargo",
      args: ["clippy", "--workspace", "--all-targets", "--locked", "--", "-D", "warnings"],
    },
  ], "workspace Clippy");
  equal(steps.filter((step) => step.args.includes("x86_64-pc-windows-gnu")), [
    {
      name: "Windows Clippy",
      command: "cargo",
      args: [
        "clippy",
        "-p",
        "adamite-io",
        "--target",
        "x86_64-pc-windows-gnu",
        "--all-targets",
        "--locked",
        "--",
        "-D",
        "warnings",
      ],
    },
  ], "Windows Clippy");
  const clippyIndex = steps.findIndex((step) => step.name === "Clippy");
  equal(steps[clippyIndex + 1]?.name, "Windows Clippy", "Windows Clippy follows Clippy");
  equal(steps.filter((step) => step.args[0] === "test" && step.args.includes("--workspace")), [
    {
      name: "Rust tests",
      command: "cargo",
      args: ["test", "--workspace", "--locked"],
      env: { PROPTEST_CASES: "1024" },
    },
  ], "workspace tests");
  equal(steps.find((step) => step.name === "Rust documentation")?.args, [
    "doc",
    "--workspace",
    "--no-deps",
  ], "documentation");
  equal(steps.filter((step) => step.args.includes("thumbv7em-none-eabihf")), [
    {
      name: "no_std build",
      command: "cargo",
      args: ["build", "-p", "adamite-ntfs", "--target", "thumbv7em-none-eabihf"],
    },
  ], "no_std build");
  for (const step of checkSteps({ packages: fuzzPackages, fuzz: true })) {
    assert(!step.args.some((arg) => arg.includes("features")), `${step.name} has feature flags`);
  }
});

Deno.test("normal checks do not install tools, invoke nightly or generate fixtures/corpora", () => {
  const steps = checkSteps({ packages: fuzzPackages, fuzz: false });
  for (const step of steps) {
    assert(step.command === "cargo" || step.command === "deno", `unexpected tool ${step.command}`);
    for (
      const token of [
        "+nightly",
        "install",
        "sudo",
        "ntfsinfo",
        "mkntfs",
        "zstd",
        "fuzz-corpus",
        "fixtures",
      ]
    ) {
      assert(!step.args.includes(token), `${step.name} invokes ${token}`);
    }
  }
});

Deno.test("fuzz checks build both packages and smoke every target with bounded flags", () => {
  const steps = checkSteps({ packages: fuzzPackages, fuzz: true });
  const installs = steps.filter((step) => step.args[0] === "install");
  equal(installs.length, 1, "cargo-fuzz installation count");
  equal(installs[0].args, ["install", "cargo-fuzz", "--locked"], "cargo-fuzz installation");
  const runs = steps.filter((step) => step.args.slice(0, 3).join(" ") === "+nightly fuzz run");
  equal(
    runs.length,
    fuzzPackages.reduce((count, pkg) => count + pkg.targets.length, 0),
    "fuzz target count",
  );
  for (const pkg of fuzzPackages) {
    const build = steps.find((step) => step.name === `fuzz build ${pkg.name}`);
    equal(build?.cwd, `crates/${pkg.name}`, `${pkg.name} build directory`);
    equal(build?.args, ["+nightly", "fuzz", "build"], `${pkg.name} build`);
    for (const target of pkg.targets) {
      const run = runs.find((step) => step.cwd === `crates/${pkg.name}` && step.args[3] === target);
      assert(run, `missing ${pkg.name}/${target} smoke`);
      equal(run.args, [
        "+nightly",
        "fuzz",
        "run",
        target,
        `../../target/fuzz-smoke/${pkg.name}/${target}/`,
        `fuzz/corpus/${target}/`,
        "--",
        "-max_total_time=30",
        "-max_len=262144",
        "-timeout=2",
        "-rss_limit_mb=2048",
      ], `${pkg.name}/${target} flags`);
      assert(steps.indexOf(installs[0]) < steps.indexOf(build!), "installation must precede build");
      assert(steps.indexOf(build!) < steps.indexOf(run), "build must precede smoke");
    }
  }
});

Deno.test("fuzz smoke writes to scratch first and reads committed seeds second", () => {
  const root = new URL("../", import.meta.url);
  const runs = checkSteps({ packages: fuzzPackages, fuzz: true }).filter((step) =>
    step.args.slice(0, 3).join(" ") === "+nightly fuzz run"
  );
  const scratchPaths = new Set<string>();
  for (const step of runs) {
    const cwd = new URL(`${step.cwd}/`, root);
    const scratch = new URL(step.args[4], cwd);
    const seeds = new URL(step.args[5], cwd);
    assert(step.scratchCorpus, `${step.name} must prepare a scratch directory`);
    equal(scratch.href, new URL(`${step.scratchCorpus}/`, root).href, "prepared output path");
    assert(
      scratch.href.startsWith(new URL("target/fuzz-smoke/", root).href),
      `${step.name} writes outside the ignored scratch tree`,
    );
    equal(seeds.href, new URL(`fuzz/corpus/${step.args[3]}/`, cwd).href, "committed seed source");
    equal(step.args[6], "--", "exactly two corpus directories");
    scratchPaths.add(scratch.href);
  }
  equal(
    scratchPaths.size,
    fuzzPackages.reduce((count, pkg) => count + pkg.targets.length, 0),
    "each smoke target has its own output directory",
  );
  assert(
    checkSteps({ packages: fuzzPackages, fuzz: false }).every((step) => !step.scratchCorpus),
    "normal checks must not reset scratch directories",
  );
});

Deno.test("scratch corpus is removed and recreated before reuse, including its first run", async () => {
  const original = { remove: Deno.remove, mkdir: Deno.mkdir };
  const calls: unknown[] = [];
  let missing = false;
  Deno.remove = (path, options) => {
    calls.push(["remove", path, options]);
    return missing ? Promise.reject(new Deno.errors.NotFound()) : Promise.resolve();
  };
  Deno.mkdir = (path, options) => {
    calls.push(["mkdir", path, options]);
    return Promise.resolve();
  };
  const path = "target/fuzz-smoke/adamite-core/scan";
  try {
    for (const firstRun of [false, true]) {
      missing = firstRun;
      calls.length = 0;
      await resetScratchCorpus(path);
      equal(calls, [
        ["remove", path, { recursive: true }],
        ["mkdir", path, { recursive: true }],
      ], "fresh corpus preparation");
    }
  } finally {
    Object.assign(Deno, original);
  }
});

Deno.test("scratch cleanup failures stop preparation", async () => {
  const original = { remove: Deno.remove, mkdir: Deno.mkdir };
  const expected = new Deno.errors.PermissionDenied("blocked");
  let created = false;
  Deno.remove = () => Promise.reject(expected);
  Deno.mkdir = () => {
    created = true;
    return Promise.resolve();
  };
  try {
    let actual: unknown;
    try {
      await resetScratchCorpus("target/fuzz-smoke/adamite-core/scan");
    } catch (error) {
      actual = error;
    }
    assert(actual === expected, "cleanup failure must propagate");
    assert(!created, "failed cleanup must not proceed to directory creation");
  } finally {
    Object.assign(Deno, original);
  }
});

Deno.test("absent fuzz packages produce no formatting, linting, builds or installation", () => {
  const steps = checkSteps({ packages: [], fuzz: true });
  assert(!steps.some((step) => step.name.includes("fuzz")), "absent package produced a fuzz step");
  const coreOnly = checkSteps({ packages: [fuzzPackages[1]], fuzz: true });
  assert(
    !coreOnly.some((step) => step.args.includes("crates/adamite-ntfs/fuzz/Cargo.toml")),
    "absent parser fuzz package was linted",
  );
  equal(
    coreOnly.filter((step) => step.args[1] === "fuzz" && step.args[2] === "run").length,
    fuzzPackages[1].targets.length,
    "core-only smoke count",
  );
});

Deno.test("fuzz preflight visits every corpus and rejects a missing or empty target in either package", async () => {
  const paths = fuzzPackages.flatMap((pkg) =>
    pkg.targets.map((target) => `crates/${pkg.name}/fuzz/corpus/${target}`)
  );
  const original = { readDir: Deno.readDir, stat: Deno.stat };
  const visited: string[] = [];
  let missing = "";
  let empty = "";
  Deno.readDir = async function* (path) {
    visited.push(String(path));
    if (path === missing) throw new Deno.errors.NotFound();
    if (path === empty) return;
    yield { name: "seed", isFile: true, isDirectory: false, isSymlink: false };
  };
  Deno.stat = () => Promise.resolve({ size: 1 } as Deno.FileInfo);
  try {
    await requireFuzzCorpora(fuzzPackages);
    equal(visited, paths, "preflight corpus paths");
    for (const path of paths) {
      for (const kind of ["missing", "empty"]) {
        missing = kind === "missing" ? path : "";
        empty = kind === "empty" ? path : "";
        let error: unknown;
        try {
          await requireFuzzCorpora(fuzzPackages);
        } catch (caught) {
          error = caught;
        }
        assert(error instanceof Error, `${kind} ${path} was accepted`);
        equal(
          error.message,
          `${kind} fuzz corpus: ${path}; run deno task fuzz-corpus`,
          "preflight failure",
        );
      }
    }
  } finally {
    Object.assign(Deno, original);
  }
});

Deno.test("corpus preflight preserves access errors", async () => {
  const original = Deno.readDir;
  const expected = new Deno.errors.PermissionDenied("blocked");
  Deno.readDir = () => {
    throw expected;
  };
  try {
    let actual: unknown;
    try {
      await requireCorpus("blocked");
    } catch (error) {
      actual = error;
    }
    assert(actual === expected, "access failure was converted into an empty-corpus error");
  } finally {
    Deno.readDir = original;
  }
});

Deno.test("fuzz target inventory matches both Cargo manifests", async () => {
  for (const pkg of fuzzPackages) {
    const manifest = parseToml(await Deno.readTextFile(`crates/${pkg.name}/fuzz/Cargo.toml`));
    const bins = manifest.bin as { name: string }[];
    equal(
      bins.map((bin) => bin.name).sort(),
      [...pkg.targets].sort(),
      `${pkg.name} target inventory`,
    );
    const tests = (manifest.test ?? []) as { name: string }[];
    equal(
      tests.map((test) => test.name).sort(),
      [...pkg.tests].sort(),
      `${pkg.name} harness inventory`,
    );
  }
});

type WorkflowStep = {
  run?: string;
  if?: string;
  "working-directory"?: string;
  with?: Record<string, string>;
  env?: Record<string, string>;
};
type WorkflowJob = {
  "runs-on": string;
  steps: WorkflowStep[];
  strategy?: { matrix: { include?: { package: string; target: string }[]; os?: string[] } };
};

Deno.test("CI covers every fuzz target, Deno tests and Windows while keeping no_std parser-only", async () => {
  const workflow = parseYaml(await Deno.readTextFile(".github/workflows/ci.yml")) as {
    jobs: Record<string, WorkflowJob>;
  };
  const jobs = workflow.jobs;
  const expected = fuzzPackages.flatMap((pkg) =>
    pkg.targets.map((target) => `${pkg.name}/${target}`)
  );
  equal(
    jobs.fuzz.strategy?.matrix.include?.map((entry) => `${entry.package}/${entry.target}`),
    expected,
    "CI fuzz matrix",
  );
  equal(jobs.fuzz["runs-on"], "ubuntu-latest", "fuzz runner");
  for (const pkg of fuzzPackages) {
    const lint = jobs.check.steps.find((step) =>
      step.run ===
        `cargo clippy --manifest-path crates/${pkg.name}/fuzz/Cargo.toml --all-targets --locked -- -D warnings`
    );
    equal(lint?.env, { RUSTFLAGS: "--cfg fuzzing" }, `${pkg.name} CI fuzz configuration`);
  }
  const smoke = jobs.fuzz.steps.find((step) => step.run?.startsWith("cargo +nightly fuzz run"));
  equal(smoke?.["working-directory"], "crates/${{ matrix.package }}", "CI fuzz directory");
  assert(
    smoke?.run?.endsWith("-- -max_total_time=30 -max_len=262144 -timeout=2 -rss_limit_mb=2048"),
    "CI fuzz flags changed",
  );
  assert(
    jobs.fuzz.steps.some((step) => step.with?.workspaces === "crates/${{ matrix.package }}/fuzz"),
    "CI fuzz cache directory changed",
  );
  assert(
    jobs.check.strategy?.matrix.os?.includes("windows-latest"),
    "missing Windows workspace checks",
  );
  equal(
    jobs.check.steps.filter((step) =>
      step.run === "cargo test -p adamite-io --test raw_volume --locked -- --ignored"
    ).map((step) => ({ run: step.run, if: step.if })),
    [{
      run: "cargo test -p adamite-io --test raw_volume --locked -- --ignored",
      if: "runner.os == 'Windows'",
    }],
    "Windows raw-volume smoke step",
  );
  equal(jobs.no_std.steps.flatMap((step) => step.run ? [step.run] : []), [
    "cargo build -p adamite-ntfs --target thumbv7em-none-eabihf",
  ], "CI no_std commands");
  equal(
    jobs.check.steps.flatMap((step) =>
      step.run?.startsWith("cargo ") && step.run.includes("--workspace") ? [step.run] : []
    ),
    [
      "cargo clippy --workspace --all-targets --locked -- -D warnings",
      "cargo test --workspace --locked",
      "cargo doc --workspace --no-deps",
    ],
    "CI workspace commands",
  );
  for (
    const step of checkSteps({ packages: fuzzPackages, fuzz: false }).filter((step) =>
      step.command === "deno" && step.args[0] === "test"
    )
  ) {
    assert(
      jobs.deno.steps.some((ci) => ci.run === `deno ${step.args.join(" ")}`),
      `CI omitted ${step.name}`,
    );
  }
  for (const job of Object.values(jobs)) {
    for (const step of job.steps) {
      if (step.run) {
        assert(
          /^(cargo|deno) [^\r\n]+$/.test(step.run),
          `CI step is not a cargo/deno one-liner: ${step.run}`,
        );
      }
    }
  }
});

Deno.test("Deno tasks permit both corpus paths without broadening executable access", async () => {
  const config = JSON.parse(await Deno.readTextFile("deno.json")) as {
    tasks: Record<string, string>;
  };
  for (const pkg of fuzzPackages) {
    assert(
      config.tasks.check.includes(`crates/${pkg.name}/fuzz/Cargo.toml`),
      `missing ${pkg.name} manifest permission`,
    );
    assert(
      config.tasks.check.includes(`crates/${pkg.name}/fuzz/corpus`),
      `missing ${pkg.name} corpus read permission`,
    );
    assert(
      config.tasks["fuzz-corpus"].includes(`crates/${pkg.name}/fuzz/corpus`),
      `missing ${pkg.name} corpus write permission`,
    );
  }
  assert(
    config.tasks.check.includes("--allow-run=cargo,deno "),
    "check executable permission changed",
  );
  equal(
    config.tasks.check.split(" ").filter((arg) => arg.startsWith("--allow-write")),
    ["--allow-write=target/fuzz-smoke"],
    "check writes only to ignored fuzz output",
  );
  assert(
    config.tasks["fuzz-corpus"].includes("--allow-run=cargo,zstd "),
    "corpus executable permission changed",
  );
});
