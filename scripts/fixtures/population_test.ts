import { withMountedImage } from "./lib.ts";
import { populate as basic } from "./populate/basic-4k.ts";
import { populate as fragmented } from "./populate/frag-512.ts";

function assert(condition: boolean, message: string) {
  if (!condition) throw new Error(message);
}

Deno.test("compression is enabled and inherited before the new file is written", async () => {
  const original = {
    Command: Deno.Command,
    mkdir: Deno.mkdir,
    lstat: Deno.lstat,
    writeFile: Deno.writeFile,
    link: Deno.link,
    remove: Deno.remove,
    open: Deno.open,
  };
  const events: string[] = [];
  let mounted = false;
  let fileSize = 0;
  let fileWrites = 0;
  class Command extends original.Command {
    constructor(private program: string, private options: Deno.CommandOptions = {}) {
      super(program, options);
    }
    override output(): Promise<Deno.CommandOutput> {
      const args = this.options.args ?? [];
      if (this.program === "sudo" && args[0] === "ntfs-3g") {
        assert(args[2].split(",").includes("compression"), "compression mount option is missing");
        assert(args[2].includes("streams_interface=windows"), "ADS option changed");
        mounted = true;
      } else if (this.program === "sudo" && args[0] === "umount") mounted = false;
      else if (this.program === "setfattr") {
        const name = args[args.indexOf("-n") + 1];
        const path = args.at(-1);
        assert(name !== "system.ntfs_reparse_data", "reparse data must be deferred");
        if (name === "system.ntfs_attrib_be") {
          assert(path !== "/fixture/compressed/c.txt", "existing file compression flag was used");
          if (path === "/fixture/compressed") {
            assert(args[args.indexOf("-v") + 1] === "0x00000810", "directory flags were lost");
            events.push("compression flag");
          }
        }
      }
      const stdout = this.program === "getfattr"
        ? new Uint8Array([0, 0, 0, 0x10])
        : new Uint8Array();
      const code = this.program === "mountpoint" && !mounted ? 32 : 0;
      return Promise.resolve({
        code,
        success: code === 0,
        signal: null,
        stdout,
        stderr: new Uint8Array(),
      });
    }
  }
  Object.assign(Deno, {
    Command,
    mkdir: (path: string) => {
      if (path === "/fixture/compressed") events.push("directory");
      return Promise.resolve();
    },
    lstat: () => Promise.resolve({ ino: 42, uid: 1000, gid: 1000 } as Deno.FileInfo),
    writeFile: (path: string, bytes: Uint8Array) => {
      if (path === "/fixture/compressed/c.txt") {
        events.push("new file");
        fileSize = bytes.length;
        fileWrites++;
      }
      return Promise.resolve();
    },
    link: () => Promise.resolve(),
    remove: () => Promise.resolve(),
    open: () =>
      Promise.resolve({
        truncate: () => Promise.resolve(),
        seek: () => Promise.resolve(0),
        write: (bytes: Uint8Array) => Promise.resolve(bytes.length),
        close: () => {},
      }),
  });
  try {
    const population = await withMountedImage("image", "/fixture", () => basic("/fixture"));
    assert(events.join(",") === "directory,compression flag,new file", "creation order changed");
    assert(
      fileWrites === 1 && fileSize === 262144,
      "compressed file must be created with all data",
    );
    assert(
      population.reparsePoints?.[0].path === "/reparse.bin",
      "deferred reparse entry was lost",
    );
    assert(!mounted, "image was not unmounted");
  } finally {
    Object.assign(Deno, original);
  }
});

Deno.test("fragmentation holds larger extents until DATA is complete, then frees space for growth", async () => {
  const original = {
    mkdir: Deno.mkdir,
    open: Deno.open,
    writeFile: Deno.writeFile,
    remove: Deno.remove,
  };
  const fill = new Set<string>();
  const sizes = new Map<string, number>();
  let filled = 0;
  let growth = 0;
  let released = false;
  Object.assign(Deno, {
    mkdir: () => Promise.resolve(),
    writeFile: (path: string) => {
      if (path.includes("/fill/")) {
        if (filled === 4000) return Promise.reject(new Error("ENOSPC"));
        filled++;
        fill.add(path);
      } else {
        assert(released, "growth started before the reservation was released");
        growth++;
      }
      return Promise.resolve();
    },
    open: (path: string) => {
      if (path.endsWith("frag.bin")) {
        assert(fill.size === 2000, "every other filler must be deleted first");
        assert(
          sizes.get("/fixture/allocation-reserve.tmp") === 376 * 512,
          "reservation size changed",
        );
      }
      sizes.set(path, 0);
      return Promise.resolve({
        write: (bytes: Uint8Array) => {
          assert(bytes.length === 512, "fragmented writes must allocate one cluster at a time");
          sizes.set(path, sizes.get(path)! + bytes.length);
          return Promise.resolve(bytes.length);
        },
        close: () => {},
      });
    },
    remove: (path: string) => {
      if (path.endsWith("allocation-reserve.tmp")) {
        assert(sizes.get("/fixture/frag.bin") === 600 * 512, "reservation was released too early");
        released = true;
      } else if (!fill.delete(path)) return Promise.reject(new Deno.errors.NotFound());
      return Promise.resolve();
    },
  });
  try {
    await fragmented("/fixture", 1);
    assert(released && growth === 1500, "temporary reservation or growth files are incomplete");
  } finally {
    Object.assign(Deno, original);
  }
});
