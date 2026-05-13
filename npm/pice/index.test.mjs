import { mkdtempSync, mkdirSync, copyFileSync, realpathSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { createRequire } from "node:module";
import { describe, expect, it } from "vitest";

const platformPackages = {
  "darwin-arm64": { pkg: "@pice/pice-darwin-arm64", bin: "pice", daemonBin: "pice-daemon" },
  "darwin-x64": { pkg: "@pice/pice-darwin-x64", bin: "pice", daemonBin: "pice-daemon" },
  "linux-arm64": { pkg: "@pice/pice-linux-arm64", bin: "pice", daemonBin: "pice-daemon" },
  "linux-x64": { pkg: "@pice/pice-linux-x64", bin: "pice", daemonBin: "pice-daemon" },
  "win32-x64": { pkg: "@pice/pice-win32-x64", bin: "pice.exe", daemonBin: "pice-daemon.exe" },
};

describe("npm binary resolver", () => {
  it("resolves CLI and daemon binaries from the platform package", () => {
    const key = `${process.platform}-${process.arch}`;
    const entry = platformPackages[key];
    if (!entry) {
      return;
    }

    const root = mkdtempSync(path.join(tmpdir(), "pice-npm-resolver-"));
    const copiedIndex = path.join(root, "index.js");
    copyFileSync(new URL("./index.js", import.meta.url), copiedIndex);

    const packageRoot = path.join(root, "node_modules", ...entry.pkg.split("/"));
    mkdirSync(packageRoot, { recursive: true });
    writeFileSync(
      path.join(packageRoot, "package.json"),
      JSON.stringify({ name: entry.pkg, version: "0.7.0" })
    );
    writeFileSync(path.join(packageRoot, entry.bin), "");
    writeFileSync(path.join(packageRoot, entry.daemonBin), "");

    const requireFromCopy = createRequire(copiedIndex);
    const resolver = requireFromCopy(copiedIndex);

    expect(resolver.getBinaryPath()).toBe(realpathSync(path.join(packageRoot, entry.bin)));
    expect(resolver.getDaemonBinaryPath()).toBe(realpathSync(path.join(packageRoot, entry.daemonBin)));
  });
});
