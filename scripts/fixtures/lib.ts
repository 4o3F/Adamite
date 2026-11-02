export type DeletedRecord = { path: string; record: string };
export type Population = {
  deleted: DeletedRecord[];
  reparsePoints?: { path: string; data: Uint8Array }[];
};

export async function command(program: string, args: string[], accepted = [0]) {
  const interactive = program === "sudo" && args[0] !== "--version" && args[0] !== "-n";
  const result = await new Deno.Command(program, {
    args,
    stdin: interactive ? "inherit" : "null",
    stdout: "piped",
    stderr: interactive ? "inherit" : "piped",
    env: { LC_ALL: "C.UTF-8", TZ: "UTC" },
  }).output();
  const output = {
    code: result.code,
    success: result.success,
    stdout: result.stdout,
    stderr: interactive ? new Uint8Array() : result.stderr,
  };
  if (!accepted.includes(output.code)) {
    throw new Error(
      `${program} ${args.join(" ")} failed (${output.code}): ${
        new TextDecoder().decode(output.stderr)
      }`,
    );
  }
  return output;
}

export async function text(program: string, args: string[]): Promise<string> {
  return new TextDecoder().decode((await command(program, args)).stdout);
}

export async function setAttribute(path: string, name: string, value: Uint8Array) {
  const hex = Array.from(value, (byte) => byte.toString(16).padStart(2, "0")).join("");
  await command("setfattr", ["-h", "-n", name, "-v", `0x${hex}`, path]);
}

export async function getAttribute(path: string, name: string): Promise<Uint8Array> {
  return (await command("getfattr", ["-h", "--only-values", "-n", name, path])).stdout;
}

export async function addFileFlags(path: string, flags: number) {
  const value = await getAttribute(path, "system.ntfs_attrib_be");
  if (value.length !== 4) throw new Error(`invalid NTFS flags on ${path}`);
  const view = new DataView(value.buffer, value.byteOffset, value.byteLength);
  view.setUint32(0, view.getUint32(0, false) | flags, false);
  await setAttribute(path, "system.ntfs_attrib_be", value);
}

export function pattern(size: number): Uint8Array {
  return Uint8Array.from({ length: size }, (_, index) => index % 251);
}

export async function writeAll(file: Deno.FsFile, bytes: Uint8Array) {
  let offset = 0;
  while (offset < bytes.length) {
    const written = await file.write(bytes.subarray(offset));
    if (written === 0) throw new Error("file write made no progress");
    offset += written;
  }
}

export async function withTemporaryDirectory<T>(
  action: (directory: string) => Promise<T>,
): Promise<T> {
  const directory = await Deno.makeTempDir({ dir: "/tmp", prefix: "adamite-fixtures-" });
  try {
    return await action(directory);
  } finally {
    await removeTemporaryDirectory(directory);
  }
}

async function removeTemporaryDirectory(directory: string) {
  const mount = `${directory}/mount`;
  try {
    await Deno.lstat(mount);
    if ((await command("mountpoint", ["-q", mount], [0, 32])).code === 0) {
      throw new Error(`mount remains active; preserving ${directory}`);
    }
  } catch (error) {
    if (!(error instanceof Deno.errors.NotFound)) throw error;
  }
  await Deno.remove(directory, { recursive: true });
}

export async function withMountedImage<T>(
  image: string,
  mount: string,
  action: () => Promise<T>,
): Promise<T> {
  await Deno.mkdir(mount);
  const owner = await Deno.lstat(mount);
  if (owner.uid === null || owner.gid === null) {
    throw new Error("fixture mounting requires Linux uid/gid");
  }
  try {
    await command("sudo", [
      "ntfs-3g",
      "-o",
      `streams_interface=windows,compression,uid=${owner.uid},gid=${owner.gid}`,
      image,
      mount,
    ]);
    return await action();
  } finally {
    await unmount(mount);
  }
}

async function unmount(mount: string) {
  if ((await command("mountpoint", ["-q", mount], [0, 32])).code !== 0) return;
  try {
    await command("sudo", ["sync", "-f", mount]);
  } finally {
    await command("sudo", ["umount", mount]);
  }
  for (let attempt = 0; attempt < 50; attempt++) {
    if ((await command("mountpoint", ["-q", mount], [0, 32])).code !== 0) return;
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
  throw new Error(`unmount did not complete: ${mount}`);
}

export async function paths(root: string): Promise<string[]> {
  const result = [root];
  for await (const entry of Deno.readDir(root)) {
    const path = `${root}/${entry.name}`;
    if (entry.isDirectory) result.push(...await paths(path));
    else result.push(path);
  }
  return result.sort();
}

export async function setFixtureTimes(root: string) {
  const bytes = new Uint8Array(32);
  const view = new DataView(bytes.buffer);
  for (let offset = 0; offset < 32; offset += 8) {
    view.setBigUint64(offset, 132223104000000000n, false);
  }
  for (const path of (await paths(root)).reverse()) {
    await setAttribute(path, "system.ntfs_times_be", bytes);
  }
}
