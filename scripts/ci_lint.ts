import { parse } from "@std/yaml";

async function main(): Promise<void> {
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

await main();
