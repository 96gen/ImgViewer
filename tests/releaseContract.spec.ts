import { spawnSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";

const root = join(import.meta.dirname, "..");

function read(path: string) {
  return readFileSync(join(root, path), "utf8");
}

describe("portable helper release contract", () => {
  it("builds HEIF only into the private helper and stages both executables", () => {
    const build = read("scripts/build-portable.ps1");

    expect(build).toContain(
      "Invoke-Checked $tauriCli build --no-bundle \"--\" \"--locked\"",
    );
    expect(build).toMatch(
      /cargo build --locked [^\r\n]*"?--package"?\s+imgviewer-codec-helper [^\r\n]*--features heic/,
    );
    expect(build).not.toMatch(
      /\$tauriCli build[^\r\n]*--features\s+heic/,
    );
    expect(build).toContain('"ImgViewer.CodecHelper.exe"');
    expect(build).toContain('"Assert-CodecBinaryBoundary.ps1"');
    expect(build).toContain("schemaVersion = 2");
    expect(build).toContain('role = "main"');
    expect(build).toContain('role = "codec-helper"');
    expect(build).toContain("protocolVersion = $codecProtocolVersion");
  });

  it("enforces binary imports and helper integrity after download", () => {
    const boundary = read("scripts/Assert-CodecBinaryBoundary.ps1");
    const verify = read("scripts/verify-portable-release.ps1");

    expect(boundary).toContain("/dependents");
    expect(boundary).toMatch(
      /ImgViewer\.exe must not reach native HEIF codecs through its import graph/,
    );
    expect(boundary).toMatch(
      /ImgViewer\.CodecHelper\.exe import graph must reach heif\.dll/,
    );
    expect(boundary).toContain("Get-ImportGraph");
    expect(boundary).toContain("protectedCodecPaths");
    expect(boundary).toContain("reachableImports");
    for (const script of [
      boundary,
      read("scripts/build-portable.ps1"),
      verify,
    ]) {
      expect(script).toMatch(/\(\?:lib\)\?/);
      expect(script).toMatch(/x265\|aom\|avif\|dav1d\|rav1e/);
    }
    const build = read("scripts/build-portable.ps1");
    expect(build).toContain("$forbiddenVcpkgPackagePattern");
    expect(build).toContain("$installedPackageNames");
    expect(build).toContain(
      "vcpkg installed unapproved HEIF/AVIF codec packages",
    );
    expect(verify).toContain(
      '"$artifactRoot/ImgViewer.CodecHelper.exe"',
    );
    expect(verify).toContain("Assert-ExecutablePayloadHashes");
    expect(verify).toContain('"MissingHelper"');
    expect(verify).toContain('"HelperHashMismatch"');
    expect(verify).toContain('Name "missing-helper"');
    expect(verify).toContain('Name "helper-hash-mismatch"');
  });

  it("covers the Rust workspace, helper binary, and native codecs in SBOM", () => {
    const sbom = read("scripts/add-native-sbom-components.ps1");
    const workflow = read(".github/workflows/portable-release.yml");
    const requiredNames = [
      "imgviewer",
      "imgviewer-codec-core",
      "imgviewer-codec-helper",
      "imgviewer-codec-protocol",
      "libheif",
      "libde265",
    ];

    for (const name of requiredNames) {
      expect(sbom).toContain(`"${name}"`);
      expect(workflow).toContain(`"${name}"`);
    }
    expect(sbom).toContain('"imgviewer:bundled-file"');
    expect(sbom).toContain('"imgviewer:codec-protocol-version"');
    expect(sbom).toContain("metadata.native.msvcRuntime");
    expect(workflow).toContain("buildMetadata.native.msvcRuntime");
    expect(workflow).toMatch(/-t js -t rust/);
    expect(workflow).toMatch(/-o \$baseSbom \./);
  });

  const windowsIt = process.platform === "win32" ? it : it.skip;
  windowsIt(
    "passes the synthetic schema, helper negative, and SBOM script checks",
    () => {
      const hashScripts = [
        "scripts/test-release-contract.ps1",
        "scripts/verify-portable-release.ps1",
        "scripts/add-native-sbom-components.ps1",
        "scripts/bootstrap-vcpkg.ps1",
        "scripts/build-portable.ps1",
      ].map(read);
      for (const script of hashScripts) {
        expect(script).toContain(
          "[System.Security.Cryptography.SHA256]::Create()",
        );
        expect(script).not.toContain("Get-FileHash");
      }

      // Keep the hosted runner's inherited PSModulePath unchanged. In
      // particular, a nested Windows PowerShell 5.1 process may inherit
      // PowerShell 7 module paths, so release hashing must not depend on
      // Microsoft.PowerShell.Utility autoloading.
      const result = spawnSync(
        "powershell.exe",
        [
          "-NoProfile",
          "-ExecutionPolicy",
          "Bypass",
          "-File",
          join(root, "scripts", "test-release-contract.ps1"),
        ],
        {
          cwd: root,
          encoding: "utf8",
          timeout: 60_000,
        },
      );

      expect(
        `${result.stdout}\n${result.stderr}`,
        `PowerShell release contract exited ${result.status}`,
      ).toContain(
        "PASS release-contract schema=2 executables=2 helper-negative=2 toolset-negative=1 import-boundary=5 sbom-required=7 helper-evidence=merged",
      );
      expect(result.status).toBe(0);
    },
    60_000,
  );

  it("does not introduce WDIO or WebDriver release dependencies", () => {
    const releaseFiles = [
      "scripts/build-portable.ps1",
      "scripts/verify-portable-release.ps1",
      "scripts/test-release-contract.ps1",
      "scripts/add-native-sbom-components.ps1",
      ".github/workflows/portable-release.yml",
    ]
      .map(read)
      .join("\n");
    expect(releaseFiles).not.toMatch(/\bwdio(?:\.js)?\b|webdriverio/i);
  });
});
