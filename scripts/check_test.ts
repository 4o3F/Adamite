import { requireCorpus } from "./check.ts";

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
