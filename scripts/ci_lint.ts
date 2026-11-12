import { parse } from "@std/yaml";

export const pluginJsonPaths = [
  ".claude-plugin/marketplace.json",
  "plugins/adamite/.claude-plugin/plugin.json",
  "plugins/adamite/.mcp.json",
] as const;

export const skillPath = "plugins/adamite/skills/disk-usage/SKILL.md";

export function validateSkillFrontmatter(text: string): void {
  const match = /^---\r?\n([\s\S]*?)\r?\n---(?:\r?\n|$)/.exec(text);
  if (!match) throw new Error("SKILL.md requires YAML frontmatter");
  const fields = parse(match[1]) as Record<string, unknown> | null;
  for (const key of ["name", "description"]) {
    if (typeof fields?.[key] !== "string" || !fields[key].trim()) {
      throw new Error(`SKILL.md requires a nonempty ${key}`);
    }
  }
}

export async function checkPlugin(
  readText: (path: string) => Promise<string> = Deno.readTextFile,
): Promise<void> {
  for (const path of pluginJsonPaths) {
    JSON.parse(await readText(path));
  }
  validateSkillFrontmatter(await readText(skillPath));
}

async function main(): Promise<void> {
  await checkPlugin();
  console.log("Valid plugin JSON and Skill frontmatter");
  const directory = ".github/workflows";
  try {
    await Deno.stat(directory);
  } catch (error) {
    if (error instanceof Deno.errors.NotFound) {
      console.log(`Skipping workflow syntax check: ${directory} does not exist yet`);
      return;
    }
    throw error;
  }
  let count = 0;
  for await (const entry of Deno.readDir(directory)) {
    if (!entry.isFile || !/\.ya?ml$/.test(entry.name)) continue;
    const path = `${directory}/${entry.name}`;
    parse(await Deno.readTextFile(path));
    console.log(`Valid YAML: ${path}`);
    count++;
  }
  console.log(`Checked ${count} workflow files`);
}

if (import.meta.main) await main();
