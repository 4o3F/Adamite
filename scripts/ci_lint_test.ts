import { checkPlugin, pluginJsonPaths, skillPath, validateSkillFrontmatter } from "./ci_lint.ts";

function assert(condition: unknown, message: string): asserts condition {
  if (!condition) throw new Error(message);
}

const validSkill = "---\nname: disk-usage\ndescription: Inspect disk usage.\n---\n# Disk usage\n";

Deno.test("plugin check reads all manifests and Skill frontmatter", async () => {
  const visited: string[] = [];
  await checkPlugin((path) => {
    visited.push(path);
    return Promise.resolve(path === skillPath ? validSkill : "{}");
  });
  assert(
    JSON.stringify(visited) === JSON.stringify([...pluginJsonPaths, skillPath]),
    "check must read each of the three JSON files and the Skill",
  );
});

Deno.test("plugin check rejects malformed or missing JSON in each manifest", async () => {
  for (const invalidPath of pluginJsonPaths) {
    for (const missing of [false, true]) {
      let rejected = false;
      try {
        await checkPlugin((path) => {
          if (path === invalidPath) {
            if (missing) return Promise.reject(new Deno.errors.NotFound(path));
            return Promise.resolve('{"name":');
          }
          return Promise.resolve(path === skillPath ? validSkill : "{}");
        });
      } catch (error) {
        rejected = missing ? error instanceof Deno.errors.NotFound : error instanceof SyntaxError;
      }
      assert(rejected, `${invalidPath} must reject ${missing ? "missing files" : "invalid JSON"}`);
    }
  }
});

Deno.test("Skill frontmatter accepts YAML strings and rejects missing or invalid fields", () => {
  validateSkillFrontmatter(validSkill);
  validateSkillFrontmatter(validSkill.replaceAll("\n", "\r\n"));
  validateSkillFrontmatter("---\nname: disk-usage\ndescription: >-\n  Inspect disk usage.\n---\n");
  for (
    const text of [
      "# No frontmatter\nname: disk-usage\ndescription: Inspect disk usage.\n",
      "---\nname: disk-usage\ndescription: Missing closing delimiter\n",
      "---\nname: disk-usage\n---\n",
      "---\ndescription: Inspect disk usage.\n---\n",
      "---\nname: ''\ndescription: Inspect disk usage.\n---\n",
      "---\nname: disk-usage\ndescription: '  '\n---\n",
      "---\nname: disk-usage\ndescription: 42\n---\n",
      "---\nnull\n---\n",
      "---\nname: [\n---\n",
    ]
  ) {
    let rejected = false;
    try {
      validateSkillFrontmatter(text);
    } catch {
      rejected = true;
    }
    assert(rejected, `invalid frontmatter accepted: ${text}`);
  }
});
