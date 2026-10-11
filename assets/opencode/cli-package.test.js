import { describe, expect, test } from "bun:test";
import { spawnSync } from "node:child_process";
import { copyFileSync, mkdtempSync, readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

const root = fileURLToPath(new URL("../../", import.meta.url));
const read = (name) => readFileSync(`${root}${name}`, "utf8");
const pkg = JSON.parse(read("package.json"));
const workflow = Bun.YAML.parse(read(".github/workflows/release.yml"));

// Runs the workflow's own version-check script in a scratch directory holding
// copies of the real manifests, so the tag rule is exercised, not re-described.
function runVersionCheck(tag) {
  const dir = mkdtempSync(join(tmpdir(), "phasegent-version-check-"));
  copyFileSync(`${root}Cargo.toml`, join(dir, "Cargo.toml"));
  copyFileSync(`${root}package.json`, join(dir, "package.json"));
  const step = workflow.jobs["version-check"].steps.find((entry) =>
    String(entry.run ?? "").includes("MAJOR.MINOR.PATCH"),
  );
  return spawnSync("bash", ["-c", step.run], {
    cwd: dir,
    encoding: "utf8",
    env: { ...process.env, GITHUB_REF_NAME: tag },
  });
}

describe("package.json plugin workspace", () => {
  test("keeps the plugin identity and build contract", () => {
    expect(pkg.name).toBe("phasegent");
    expect(pkg.private).toBe(true);
    expect(pkg.type).toBe("module");
    expect(pkg.scripts["build:plugin"]).toBe("bun assets/opencode/src/build.js");
  });

  test("runs the plugin tests and keeps the test alias on them", () => {
    expect(pkg.scripts["test:plugin"]).toBe("bun test assets/opencode");
    expect(pkg.scripts.test).toBe("bun run test:plugin");
  });

  test("declares no desktop, frontend, or Electron surface", () => {
    expect(pkg).not.toHaveProperty("main");
    for (const name of Object.keys(pkg.scripts)) {
      expect(name).not.toMatch(/electron|dev|validate|typecheck|lint|dist|build-only/);
    }
    const deps = { ...pkg.dependencies, ...pkg.devDependencies };
    for (const name of Object.keys(deps)) {
      expect(name).not.toMatch(/electron|vue|vite|nuxt|tailwind|eslint|typescript|^@types\//);
    }
  });
});

describe("release workflow structure", () => {
  const build = workflow.jobs.build;
  const release = workflow.jobs.release;

  test("builds four native CLI targets with the full feature set", () => {
    const targets = build.strategy.matrix.include.map((entry) => entry.target);
    expect(targets).toEqual([
      "x86_64-unknown-linux-gnu",
      "aarch64-unknown-linux-gnu",
      "aarch64-apple-darwin",
      "x86_64-pc-windows-msvc",
    ]);
    const cargoBuild = build.steps.find((step) => step.name === "Build release binary");
    expect(cargoBuild.run).toContain("--features postgres,notify-dingtalk,notify-email");
  });

  test("gates the build on the strict numeric version check", () => {
    expect(build.needs).toBe("version-check");
    const check = workflow.jobs["version-check"].steps.find((step) =>
      String(step.run ?? "").includes("MAJOR.MINOR.PATCH"),
    );
    expect(check.run).toContain("tomllib");
    expect(check.run).toContain("package.json");
  });

  test("keeps least-privilege permissions and publishes SHA256SUMS", () => {
    expect(workflow.permissions).toEqual({ contents: "read" });
    expect(release.permissions).toEqual({ contents: "write" });
    const sums = release.steps.find((step) => step.name === "Generate SHA256SUMS");
    expect(sums.run).toContain("sha256sum phasegent-* > SHA256SUMS");
    const publish = release.steps.find((step) => step.name === "Publish GitHub release");
    expect(publish.with.files).toContain("dist/SHA256SUMS");
  });

  test("builds and publishes only the Windows CLI MSI", () => {
    const wix = build.steps.find((step) => step.name === "Install pinned WiX CLI");
    expect(wix.if).toBe("runner.os == 'Windows'");
    expect(wix.run).toBe("dotnet tool install --global wix --version 6.0.2");

    const regressions = build.steps.find((step) => step.name === "Test Windows MSI PowerShell regressions");
    expect(regressions.if).toBe("runner.os == 'Windows'");
    expect(regressions.shell).toBe("pwsh");
    expect(regressions.run).toContain("pwsh -NoProfile -File tests/windows_installer/queries.ps1");
    expect(regressions.run).toContain("pwsh -NoProfile -File tests/windows_installer/rows.ps1");
    expect(regressions.run).toContain("pwsh -NoProfile -File tests/windows_installer/cleanup.ps1");
    // An unescaped MSI query only fails inside the real checker, after a full
    // WiX compile, so the query contract runs first as a cheap preflight.
    const queries = "pwsh -NoProfile -File tests/windows_installer/queries.ps1";
    const rows = "pwsh -NoProfile -File tests/windows_installer/rows.ps1";
    expect(regressions.run.indexOf(queries)).toBeLessThan(regressions.run.indexOf(rows));

    const msiBuild = build.steps.find((step) => step.name === "Build and validate Windows MSI");
    expect(msiBuild.if).toBe("runner.os == 'Windows'");
    expect(msiBuild.run).toContain("scripts/build-windows-msi.ps1");
    expect(msiBuild.run).toContain("$env:GITHUB_REF_NAME");
    expect(build.steps.indexOf(regressions)).toBeLessThan(build.steps.indexOf(msiBuild));

    const upload = build.steps.find((step) => step.name === "Upload artifact (Windows MSI)");
    expect(upload.if).toBe("runner.os == 'Windows'");
    expect(upload.with.path).toBe("phasegent-${{ github.ref_name }}-${{ matrix.target }}.msi");

    const msiSteps = build.steps.filter((step) =>
      `${step.name ?? ""} ${step.run ?? ""} ${step.with?.path ?? ""}`.toLowerCase().includes("msi") ||
      `${step.name ?? ""} ${step.run ?? ""}`.toLowerCase().includes("wix"),
    );
    for (const step of msiSteps) expect(step.if).toBe("runner.os == 'Windows'");

    const publish = release.steps.find((step) => step.name === "Publish GitHub release");
    expect(publish.with.files).toContain("dist/phasegent-*-x86_64-pc-windows-msvc.msi");
    expect(publish.with.files).not.toMatch(/\.dmg|\.pkg/i);
  });

  test("does not restore Electron or macOS desktop packaging", () => {
    const steps = [...build.steps, ...release.steps].map((step) =>
      `${step.name ?? ""} ${step.run ?? ""} ${step.uses ?? ""}`.toLowerCase(),
    );
    for (const text of steps) {
      expect(text).not.toMatch(/bun install|setup-bun|electron|electron-builder|\.dmg|\.pkg|frontend|dist-hash/);
    }
  });
});

describe("version parity", () => {
  test("package.json version mirrors the Cargo package version", () => {
    const cargo = read("Cargo.toml");
    const match = cargo.match(/^\[package\][\s\S]*?^version\s*=\s*"([^"]+)"/m);
    expect(match).not.toBeNull();
    expect(pkg.version).toBe(match[1]);
    expect(pkg.version).toMatch(/^\d+\.\d+\.\d+$/);
  });

  test("the workflow version-check accepts the matching strict tag", () => {
    const result = runVersionCheck(`v${pkg.version}`);
    expect(result.status, result.stderr).toBe(0);
    expect(result.stdout).toContain(`matches Cargo.toml and package.json (${pkg.version})`);
  });

  test("the workflow version-check rejects non-strict and mismatched tags", () => {
    for (const tag of ["v2.24", "v2.24.0-rc1", `v9.9.9`, pkg.version]) {
      const result = runVersionCheck(tag);
      expect(result.status, `tag ${tag} must fail`).not.toBe(0);
    }
  });
});
