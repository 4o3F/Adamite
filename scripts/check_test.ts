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

Deno.test("normal checks include fixture, check and package tests with scoped permissions", () => {
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
    ["--allow-read=deno.json,.github/workflows,crates/adamite-ntfs/fuzz/Cargo.toml,crates/adamite-core/fuzz/Cargo.toml"],
    "check-script permissions",
  );
  equal(
    steps.find((step) => step.name === "package script tests"),
    {
      name: "package script tests",
      command: "deno",
      args: [
        "test",
        "--frozen",
        "--allow-read=target/package-test",
        "--allow-write=target/package-test",
        "scripts/package_test.ts",
      ],
    },
    "package tests and temporary-tree permissions",
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
        "-p",
        "adamite-cli",
        "-p",
        "adamite-tui",
        "-p",
        "adamite-profile",
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
  uses?: string;
  run?: string;
  if?: string;
  "working-directory"?: string;
  with?: Record<string, string | boolean | number>;
  env?: Record<string, string>;
};
type WorkflowJobSettings = {
  needs?: string | string[];
  permissions?: Record<string, string>;
};
type StepsJob = WorkflowJobSettings & {
  "runs-on": string;
  uses?: never;
  steps: WorkflowStep[];
  strategy?: { matrix: { include?: { package: string; target: string }[]; os?: string[] } };
};
type ReusableWorkflowJob = WorkflowJobSettings & {
  uses: string;
  steps?: never;
};
type WorkflowJob = StepsJob | ReusableWorkflowJob;
type Workflow = {
  on: Record<string, unknown>;
  permissions?: Record<string, string>;
  concurrency?: { group: string; "cancel-in-progress": boolean };
  jobs: Record<string, WorkflowJob>;
};

async function readWorkflow(path: string): Promise<Workflow> {
  return parseYaml(await Deno.readTextFile(path)) as Workflow;
}

function stepsJob(workflow: Workflow, name: string): StepsJob {
  const job = workflow.jobs[name];
  assert(job && job.uses === undefined, `${name} must be an ordinary job with steps`);
  assert(Array.isArray(job.steps), `${name} must have steps`);
  return job;
}

function assertOneLineRuns(workflow: Workflow, path: string): void {
  for (const [name, job] of Object.entries(workflow.jobs)) {
    const context = `${path}: ${name}`;
    if (job.uses !== undefined) {
      assert(typeof job.uses === "string", `${context} must name a reusable workflow`);
      equal(job.steps, undefined, `${context} reusable-workflow job cannot have steps`);
      continue;
    }
    assert(Array.isArray(job.steps), `${context} must have steps`);
    for (const step of job.steps) {
      if (step.run !== undefined) {
        assert(
          /^(cargo|deno) [^\r\n]+$/.test(step.run) && !/[\r\n]/.test(step.run),
          `${context} step is not a cargo/deno one-liner: ${step.run}`,
        );
      }
    }
  }
}

Deno.test("every workflow's ordinary jobs use cargo/deno run one-liners", async () => {
  const paths: string[] = [];
  for await (const entry of Deno.readDir(".github/workflows")) {
    if (!entry.isFile || !/\.ya?ml$/.test(entry.name)) continue;
    const path = `.github/workflows/${entry.name}`;
    assertOneLineRuns(await readWorkflow(path), path);
    paths.push(path);
  }
  for (const name of ["ci", "release"]) {
    assert(paths.includes(`.github/workflows/${name}.yml`), `${name} workflow was not checked`);
  }
});

Deno.test("run rules allow reusable-workflow jobs and still check ordinary jobs beside them", () => {
  const workflow = parseYaml(`
jobs:
  ci:
    uses: ./.github/workflows/ci.yml
  publish:
    runs-on: ubuntu-latest
    steps:
      - uses: denoland/setup-deno@v2
      - run: deno task release-check v0.1.0
`) as Workflow;
  assertOneLineRuns(workflow, "test.yml");
  const publish = stepsJob(workflow, "publish");
  for (const run of ["echo shell", "", "cargo test\ndeno lint", "cargo test\n"]) {
    publish.steps = [{ run }];
    let rejected = false;
    try {
      assertOneLineRuns(workflow, "test.yml");
    } catch (error) {
      rejected = error instanceof Error && error.message.includes("test.yml: publish");
    }
    assert(rejected, `accepted invalid run: ${JSON.stringify(run)}`);
  }
});

Deno.test("CI covers every fuzz target, Deno tests and Windows while keeping no_std parser-only", async () => {
  const workflow = await readWorkflow(".github/workflows/ci.yml");
  const jobs = {
    check: stepsJob(workflow, "check"),
    fuzz: stepsJob(workflow, "fuzz"),
    no_std: stepsJob(workflow, "no_std"),
    deno: stepsJob(workflow, "deno"),
  };
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
});

Deno.test("CI is callable, packages both runners after tests and uploads their artifacts", async () => {
  const workflow = await readWorkflow(".github/workflows/ci.yml");
  equal(workflow.on, {
    push: { branches: ["main"] },
    pull_request: null,
    workflow_call: null,
  }, "CI triggers preserve branch and PR runs without tag runs");

  const check = stepsJob(workflow, "check");
  equal(check.strategy?.matrix.os, ["ubuntu-latest", "windows-latest"], "packaging runners");
  equal(check["runs-on"], "${{ matrix.os }}", "check uses the runner matrix");
  const cliSmoke = "cargo test -p adamite-cli --test raw_volume --locked -- --ignored";
  equal(check.steps.filter((step) => step.run === cliSmoke), [{
    run: cliSmoke,
    if: "runner.os == 'Windows'",
  }], "Windows elevated CLI smoke");

  const setup = { uses: "denoland/setup-deno@v2", with: { "deno-version": "2.9.8", cache: true } };
  const deno = stepsJob(workflow, "deno");
  equal(deno.steps.filter((step) => step.uses === setup.uses), [setup], "pinned Deno setup");
  const setupIndex = check.steps.findIndex((step) => step.uses === setup.uses);
  assert(setupIndex >= 0, "check matrix must install Deno");
  equal(check.steps.slice(setupIndex, setupIndex + 4), [
    setup,
    { run: "cargo build --release -p adamite-cli --locked" },
    { run: "deno task package ${{ runner.os }}" },
    {
      uses: "actions/upload-artifact@v7",
      with: {
        name: "adamite-${{ runner.os }}",
        path: "dist/",
        "if-no-files-found": "error",
      },
    },
  ], "release build, package and upload sequence");
  for (const run of ["cargo test --workspace --locked", cliSmoke]) {
    const index = check.steps.findIndex((step) => step.run === run);
    assert(index >= 0 && index < setupIndex, `${run} must precede packaging`);
  }

  for (const [name, job] of Object.entries(workflow.jobs)) {
    if (job.uses !== undefined) continue;
    for (const [index, step] of job.steps.entries()) {
      if (!step.run?.startsWith("deno task package ")) continue;
      const setups = job.steps.slice(0, index).filter((step) => step.uses === setup.uses);
      equal(setups, [setup], `${name} must install pinned Deno before packaging`);
    }
  }
});

Deno.test("release checks CI before publishing its merged artifacts without rebuilding", async () => {
  const release = await readWorkflow(".github/workflows/release.yml");
  const ci = await readWorkflow(".github/workflows/ci.yml");
  equal(release.on, { push: { tags: ["v*"] } }, "release tag filter");
  equal(release.permissions, ci.permissions, "release and CI default permissions");
  equal(release.permissions, { contents: "read" }, "read-only default");
  assert(!("concurrency" in release), "release cannot share the called CI's concurrency group");
  for (const [name, job] of Object.entries(release.jobs)) {
    assert(!("concurrency" in job), `${name} must not acquire a caller concurrency group`);
  }
  equal(ci.concurrency, {
    group: "${{ github.workflow }}-${{ github.ref }}",
    "cancel-in-progress": true,
  }, "only the called CI acquires the concurrency group");
  equal(Object.keys(release.jobs).sort(), ["ci", "publish"], "release job inventory");
  equal(release.jobs.ci, {
    uses: "./.github/workflows/ci.yml",
    permissions: { contents: "read" },
  }, "reusable CI job");

  const check = stepsJob(ci, "check");
  const checkout = check.steps.find((step) => step.uses?.startsWith("actions/checkout@"));
  const setup = check.steps.find((step) => step.uses?.startsWith("denoland/setup-deno@"));
  const upload = check.steps.find((step) => step.uses?.startsWith("actions/upload-artifact@"));
  assert(checkout && setup && upload, "CI must check out, install Deno and upload artifacts");
  const uploadMajor = upload.uses?.match(/^actions\/upload-artifact@(v\d+)$/)?.[1];
  assert(uploadMajor, "upload-artifact must be pinned to a major");
  equal(upload.with?.name, "adamite-${{ runner.os }}", "artifact names selected by adamite-*");
  equal(
    check.strategy?.matrix.os,
    ["ubuntu-latest", "windows-latest"],
    "Linux and Windows artifacts",
  );

  const publish = stepsJob(release, "publish");
  equal(publish.needs, "ci", "publication waits for all CI jobs");
  equal(publish.permissions, { contents: "write" }, "publication permissions");
  equal(publish["runs-on"], "ubuntu-latest", "publication runner");
  const ghRelease = publish.steps.at(-1);
  assert(
    ghRelease && /^softprops\/action-gh-release@v\d+$/.test(ghRelease.uses ?? ""),
    "gh-release must be pinned to a major",
  );
  equal(publish.steps, [
    checkout,
    setup,
    { run: "deno task release-check ${{ github.ref_name }}" },
    {
      uses: `actions/download-artifact@${uploadMajor}`,
      with: { pattern: "adamite-*", "merge-multiple": true, path: "dist/" },
    },
    {
      uses: ghRelease.uses,
      with: { files: "dist/*", fail_on_unmatched_files: true },
    },
  ], "checkout, identical Deno setup, tag validation, merged downloads and publication only");
  assertOneLineRuns(release, ".github/workflows/release.yml");
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

Deno.test("packaging tasks limit file access and pin the tar dependency exactly", async () => {
  const config = JSON.parse(await Deno.readTextFile("deno.json")) as {
    tasks: Record<string, string>;
    imports: Record<string, string>;
  };
  for (
    const [task, permissions] of [
      ["package", [
        "--allow-read=crates/adamite-cli/Cargo.toml,target/release/adamite,target/release/adamite.exe",
        "--allow-write=dist",
      ]],
      ["release-check", ["--allow-read=crates/adamite-cli/Cargo.toml"]],
    ] as const
  ) {
    const args = config.tasks[task].split(" ");
    equal(args.slice(0, 3), ["deno", "run", "--frozen"], `${task} freezes dependencies`);
    equal(
      args.filter((arg) => arg.startsWith("--allow-") || arg === "-A"),
      permissions,
      `${task} permissions`,
    );
    equal(
      args.slice(3 + permissions.length),
      task === "package" ? ["scripts/package.ts"] : ["scripts/package.ts", "--release-check"],
      `${task} entry point`,
    );
  }
  assert(
    /^jsr:@std\/tar@\d+\.\d+\.\d+$/.test(config.imports["@std/tar"]),
    "tar dependency must use an exact version",
  );
});
