import { execFileSync } from "node:child_process";
import { readFileSync, readdirSync, statSync } from "node:fs";
import { join, relative } from "node:path";
import { describe, expect, it } from "vitest";

const root = join(import.meta.dirname, "..");

function read(path: string) {
  return readFileSync(join(root, path), "utf8");
}

function filesBelow(path: string): string[] {
  const absolute = join(root, path);
  return readdirSync(absolute).flatMap((entry) => {
    const candidate = join(absolute, entry);
    return statSync(candidate).isDirectory()
      ? filesBelow(relative(root, candidate))
      : [relative(root, candidate)];
  });
}

function beforeTestModule(source: string) {
  return source.split(/\r?\n#\[cfg\(test\)\]\r?\n/)[0];
}

describe("desktop security contract", () => {
  it("keeps the main window capability on an exact least-privilege allowlist", () => {
    const capability = JSON.parse(read("src-tauri/capabilities/main.json"));
    expect(capability.windows).toEqual(["main"]);
    expect(capability.platforms).toEqual(["windows"]);
    expect(capability.remote).toBeUndefined();
    expect(capability.permissions).toEqual([
      "core:event:allow-listen",
      "core:event:allow-unlisten",
      "dialog:allow-open",
      "allow-open-path",
      "allow-navigate",
      "allow-current-snapshot",
      "allow-read-render",
    ]);

    const buildScript = read("src-tauri/build.rs");
    const declaredCommands = [
      ...buildScript.matchAll(/"(open_path|navigate|current_snapshot|read_render)"/g),
    ].map((match) => match[1]);
    expect(declaredCommands).toEqual([
      "open_path",
      "navigate",
      "current_snapshot",
      "read_render",
    ]);
  });

  it("keeps production CSP local and disables broad Tauri exposure", () => {
    const config = JSON.parse(read("src-tauri/tauri.conf.json"));
    expect(config.app.security.capabilities).toEqual(["main-window"]);
    expect(config.app.security.freezePrototype).toBe(true);
    expect(config.app.security.csp).toBe(
      "default-src 'self'; connect-src ipc: http://ipc.localhost; img-src 'self' blob:; style-src 'self' 'unsafe-inline'; script-src 'self'; object-src 'none'; base-uri 'none'; frame-src 'none'",
    );
    expect(config.app.security.assetProtocol).toBeUndefined();
    expect(config.app.security.dangerousRemoteDomainIpcAccess).toBeUndefined();
    expect(config.app.security.useHttpsScheme).toBeUndefined();
    expect(config.app.withGlobalTauri).not.toBe(true);
  });

  it("keeps Tauri plugins on the exact approved allowlist", () => {
    const packageManifest = JSON.parse(read("package.json"));
    const nodeDependencies = Object.keys({
      ...packageManifest.dependencies,
      ...packageManifest.devDependencies,
    });
    expect(
      nodeDependencies.filter((name) => name.startsWith("@tauri-apps/plugin-")),
    ).toEqual(["@tauri-apps/plugin-dialog"]);
    expect(nodeDependencies.join("\n")).not.toMatch(
      /(wdio|webdriverio|pdfjs|pdf-lib)/i,
    );

    const cargoManifest = read("src-tauri/Cargo.toml");
    const rustPlugins = [
      ...cargoManifest.matchAll(/^(tauri-plugin-[a-z0-9-]+)\s*=/gm),
    ]
      .map((match) => match[1])
      .sort();
    expect(rustPlugins).toEqual([
      "tauri-plugin-dialog",
      "tauri-plugin-single-instance",
      "tauri-plugin-window-state",
    ]);
    expect(cargoManifest).not.toMatch(/(pdfium|lopdf|pdf-extract)/i);

    const rustEntry = read("src-tauri/src/lib.rs");
    const registrations = [
      ...rustEntry.matchAll(
        /\.plugin\(\s*(tauri_plugin_[a-z0-9_]+)/g,
      ),
    ]
      .map((match) => match[1])
      .sort();
    expect(registrations).toEqual([
      "tauri_plugin_dialog",
      "tauri_plugin_single_instance",
      "tauri_plugin_window_state",
    ]);
    expect(rustEntry.match(/\.plugin\(/g)).toHaveLength(5);
    expect(rustEntry).toContain(
      ".plugin(navigation::navigation_policy_plugin())",
    );
    expect(rustEntry).toContain(".plugin(window::state_preflight_plugin())");

    const navigationPolicy = read("src-tauri/src/navigation.rs");
    expect(navigationPolicy).toContain(".on_navigation(");
    expect(navigationPolicy).toContain('webview.label() == "main"');
    expect(navigationPolicy).toContain(
      'url.host_str() == Some("tauri.localhost")',
    );
    expect(navigationPolicy).toContain(
      'url.host_str() == Some("127.0.0.1")',
    );
    expect(navigationPolicy).toContain('"https://tauri.localhost/"');
    for (const deniedScheme of ["file:", "data:", "blob:"]) {
      expect(navigationPolicy).toContain(`"${deniedScheme}`);
    }
  });

  it("keeps the private codec helper workspace isolated from desktop capabilities", () => {
    const cargoManifest = read("src-tauri/Cargo.toml");
    const workspaceSection = cargoManifest
      .split("[workspace]")[1]
      .split(/\r?\n\[/)[0];
    const workspaceMembers = [
      ...workspaceSection.matchAll(
        /"(crates\/codec-(?:core|helper|protocol))"/g,
      ),
    ].map((match) => match[1]);
    expect(workspaceMembers.sort()).toEqual([
      "crates/codec-core",
      "crates/codec-helper",
      "crates/codec-protocol",
    ]);
    expect(cargoManifest).toContain('default-members = ["."]');

    const coreManifest = read("src-tauri/crates/codec-core/Cargo.toml");
    const protocolManifest = read(
      "src-tauri/crates/codec-protocol/Cargo.toml",
    );
    const helperManifest = read("src-tauri/crates/codec-helper/Cargo.toml");
    const rootDependencySection = cargoManifest
      .split("[dependencies]")[1]
      .split(/\r?\n\[/)[0];
    const coreDependencySection = coreManifest
      .split("[dependencies]")[1]
      .split(/\r?\n\[/)[0];
    const coreDependencies = [
      ...coreDependencySection.matchAll(/^([a-z0-9_-]+)\s*=/gm),
    ].map((match) => match[1]);
    expect(coreDependencies).toEqual([
      "image",
      "libheif-rs",
      "libheif-sys",
      "moxcms",
      "png",
      "serde",
      "serde_json",
    ]);
    expect(rootDependencySection).not.toMatch(/^libheif-(?:rs|sys)\s*=/m);
    expect(coreManifest).toMatch(
      /^libheif-rs\s*=\s*\{[^}]*optional\s*=\s*true[^}]*\}$/m,
    );
    expect(coreManifest).toMatch(
      /^libheif-sys\s*=\s*\{[^}]*optional\s*=\s*true[^}]*\}$/m,
    );
    const helperDependencySection = helperManifest
      .split("[dependencies]")[1]
      .split(/\r?\n\[/)[0];
    const helperDependencies = [
      ...helperDependencySection.matchAll(/^([a-z0-9_-]+)\s*=/gm),
    ].map((match) => match[1]);
    expect(helperDependencies).toEqual([
      "imgviewer-codec-core",
      "imgviewer-codec-protocol",
    ]);

    const protocolSource = read(
      "src-tauri/crates/codec-protocol/src/lib.rs",
    );
    const helperLibrary = read("src-tauri/crates/codec-helper/src/lib.rs");
    const helperMain = read("src-tauri/crates/codec-helper/src/main.rs");
    const helperHandleAdapter = read(
      "src-tauri/crates/codec-helper/src/windows_handle.rs",
    );
    const helperSource = [
      protocolSource,
      helperLibrary,
      helperMain,
      helperHandleAdapter,
    ].join("\n");
    expect(protocolSource).toContain("#![forbid(unsafe_code)]");
    expect(helperMain).toContain("#![forbid(unsafe_code)]");
    expect(helperLibrary).toContain("#![deny(unsafe_code)]");
    expect(helperLibrary).toMatch(
      /#\[allow\(\s*unsafe_code,\s*reason = "the explicit Windows handle adapter owns all transferred raw handles"\s*\)\]\s*mod windows_handle;/,
    );
    expect(
      `${coreManifest}\n${protocolManifest}\n${helperManifest}\n${helperSource}`,
    ).not.toMatch(
      /\b(?:tauri|reqwest|hyper|ureq|curl|tokio|async_std|smol)\b|https?:\/\/|std::process::Command|cmd\.exe|powershell/i,
    );
    const helperProductionSource = [
      beforeTestModule(helperLibrary),
      helperMain,
      beforeTestModule(helperHandleAdapter),
    ].join("\n");
    expect(helperProductionSource).not.toMatch(
      /\b(?:Path|PathBuf|OpenOptions)\b|File::open/,
    );
    expect(helperProductionSource).toContain(
      "File::from_raw_handle(handle)",
    );
    expect(helperProductionSource).toContain(
      "take_disk_file(request.duplicated_handle, request.expected_length)",
    );
    expect(helperSource).toContain("validate_cli_arguments(std::env::args_os())");
    expect(helperSource).toContain("CliError::UnexpectedArgument");

    const ci = read(".github/workflows/ci.yml");
    for (const command of [
      ...ci.matchAll(/run:\s*(cargo (?:clippy|test)[^\r\n]*)/g),
    ].map((match) => match[1])) {
      expect(command, `Workspace crate 未納入 CI：${command}`).toContain(
        "--workspace",
      );
    }
  });

  it("pins native codec builds and packaged runtimes to MSVC v143", () => {
    const triplet = read("vcpkg-triplets/x64-windows.cmake");
    const configuration = JSON.parse(read("vcpkg-configuration.json"));
    const installer = read("scripts/install-native-deps.ps1");
    const build = read("scripts/build-portable.ps1");
    const dependencyResolver = read(
      "scripts/Resolve-NativeDependencies.ps1",
    );
    const releaseWorkflow = read(".github/workflows/portable-release.yml");

    expect(configuration["overlay-triplets"]).toEqual([
      "vcpkg-triplets",
    ]);
    expect(triplet).toMatch(
      /^set\(VCPKG_PLATFORM_TOOLSET v143\)$/m,
    );
    expect(triplet).toMatch(/^set\(VCPKG_CRT_LINKAGE dynamic\)$/m);
    expect(triplet).toMatch(/^set\(VCPKG_LIBRARY_LINKAGE dynamic\)$/m);
    expect(installer).toContain('"--overlay-triplets=$overlayTriplets"');
    expect(installer).toContain(
      "VCPKG_PLATFORM_TOOLSET\\s+v143",
    );
    expect(build).toContain('"Microsoft.VC143.CRT"');
    expect(build).toContain("& $vswhere -all -products *");
    expect(build).toContain('platformToolset = "v143"');
    expect(build).toContain("app-local=1");
    expect(dependencyResolver).toContain('"Microsoft.VC143.CRT"');
    expect(dependencyResolver).toContain("& $vswhere -all -products *");
    expect(releaseWorkflow).toContain(
      "'vcpkg-configuration.json', 'vcpkg-triplets/**'",
    );
  });

  it("keeps unsafe Rust inside reviewed Win32 and codec FFI adapters", () => {
    const rustPaths = [
      ...filesBelow("src-tauri/src"),
      ...filesBelow("src-tauri/crates"),
    ].filter((path) => path.endsWith(".rs"));
    const unsafeFiles = rustPaths
      .filter((path) => /\bunsafe\s*(?:\{|extern\b)/.test(read(path)))
      .map((path) => path.replaceAll("\\", "/"))
      .sort();
    expect(unsafeFiles).toEqual(
      [
        "src-tauri/crates/codec-core/src/heif_ffi_adapter.rs",
        "src-tauri/crates/codec-helper/src/windows_handle.rs",
        "src-tauri/src/catalog.rs",
        "src-tauri/src/codec_helper/windows.rs",
        "src-tauri/src/window.rs",
      ].sort(),
    );

    for (const path of unsafeFiles) {
      const lines = read(path).split(/\r?\n/);
      for (const [index, line] of lines.entries()) {
        if (!/\bunsafe\s*(?:\{|extern\b)/.test(line)) {
          continue;
        }
        const safetyContext = lines
          .slice(Math.max(0, index - 4), index)
          .join("\n");
        expect(
          safetyContext,
          `${path}:${index + 1} 缺少就近 SAFETY 理由`,
        ).toContain("SAFETY:");
      }
    }

    expect(read("src-tauri/src/lib.rs")).toContain(
      "#![deny(unsafe_code)]",
    );
    expect(read("src-tauri/crates/codec-core/src/lib.rs")).toContain(
      "#![deny(unsafe_code)]",
    );
    expect(read("src-tauri/crates/codec-helper/src/lib.rs")).toContain(
      "#![deny(unsafe_code)]",
    );
    expect(read("src-tauri/crates/codec-protocol/src/lib.rs")).toContain(
      "#![forbid(unsafe_code)]",
    );
  });

  it("pins the local and CI toolchains and uses locked Cargo graphs", () => {
    expect(read(".node-version").trim()).toBe("24.18.0");
    const rustToolchain = read("rust-toolchain.toml");
    expect(rustToolchain).toMatch(/^channel\s*=\s*"1\.88\.0"$/m);

    const workflows = filesBelow(".github/workflows")
      .filter((path) => /\.ya?ml$/i.test(path))
      .map(read)
      .join("\n");
    const rustActionUses = [
      ...workflows.matchAll(
        /uses:\s*dtolnay\/rust-toolchain@[0-9a-f]{40}([\s\S]*?)(?=\n\s*-\s+name:|\n\S|$)/g,
      ),
    ];
    expect(rustActionUses.length).toBeGreaterThan(0);
    for (const use of rustActionUses) {
      expect(use[1]).toMatch(/\btoolchain:\s*1\.88\.0\b/);
    }
    for (const command of [
      ...workflows.matchAll(/run:\s*(cargo (?:clippy|test)[^\r\n]*)/g),
    ].map((match) => match[1])) {
      expect(command, `Cargo graph 未鎖定：${command}`).toContain("--locked");
    }
    expect(workflows).not.toMatch(
      /cargo (?:clippy|test)(?![^\r\n]*--locked)[^\r\n]*/g,
    );
    expect(workflows).toContain(
      "check advisories bans licenses sources",
    );
    expect(workflows).toContain(
      "command-arguments: advisories bans licenses sources",
    );

    const portableBuild = read("scripts/build-portable.ps1");
    expect(portableBuild).toContain(
      'node_modules\\.bin\\tauri.cmd',
    );
    expect(portableBuild).not.toMatch(/\bpnpm(?:\.Source)?\s+exec\s+tauri\b/i);
    expect(portableBuild).toMatch(
      /if \(\$ReleaseMode -or \$FreshNative\) \{[\s\S]*?Invoke-Checked cargo clean --manifest-path \(Join-Path \$repoRoot "src-tauri\\Cargo\.toml"\)\s*\} else \{[\s\S]*?Invoke-Checked cargo clean --manifest-path \(Join-Path \$repoRoot "src-tauri\\Cargo\.toml"\) "--package" libheif-sys\s*\}/,
    );
    expect(read(".github/workflows/portable-release.yml")).toContain(
      "-SkipNativeSmoke -FreshNative",
    );
    expect(portableBuild).toContain("Get-MsvcRedistDirectories");
    expect(portableBuild).toContain("Assert-NativeLibraryLoadable");
    expect(portableBuild).toContain(
      "PASS native-test-loader dlls=2 msvc-runtime=v143 app-local=1",
    );
    const portableVerify = read("scripts/verify-portable-release.ps1");
    expect(portableVerify).toContain('"$artifactRoot/libx265.dll"');
    expect(portableVerify).toContain(
      'Assert-ExpectedFailure -Name "forbidden-codec-lib-prefix"',
    );
  });

  it("preserves hard image and allocation limits in the Rust core", () => {
    const rust = [
      ...filesBelow("src-tauri/src"),
      ...filesBelow("src-tauri/crates/codec-core/src"),
    ]
      .filter((path) => path.endsWith(".rs"))
      .map(read)
      .join("\n");
    expect(rust).toMatch(/MAX_INPUT_BYTES[^=]*=[^;]*256 \* 1024 \* 1024/);
    expect(rust).toMatch(/MAX_SIDE[^=]*=[^;]*32_768/);
    expect(rust).toMatch(/MAX_PIXELS[^=]*=[^;]*100_000_000/);
    expect(rust).toMatch(/MAX_DECODE_BYTES[^=]*=[^;]*512 \* 1024 \* 1024/);
    expect(rust).toMatch(/MAX_ANIMATION_FRAMES[^=]*=[^;]*10_000/);
    expect(rust).toMatch(
      /MAX_ANIMATION_PIXELS[^=]*=[^;]*1_000_000_000/,
    );
    expect(rust).toMatch(
      /MAX_ICC_PROFILE_BYTES[^=]*=[^;]*16 \* 1024 \* 1024/,
    );
    expect(rust).toMatch(/MAX_WINDOW_STATE_BYTES[^=]*=[^;]*64 \* 1024/);
    expect(rust).toMatch(/MAX_DIRECTORY_ENTRIES[^=]*=[^;]*100_000/);
    expect(rust).toMatch(/MAX_CATALOG_FILES[^=]*=[^;]*20_000/);
    expect(rust).toContain("normalization_working_set_bytes");
    expect(rust).toContain("source_color_type.bytes_per_pixel()");
    expect(rust.match(/validate_normalization_working_set\(/g)?.length).toBeGreaterThanOrEqual(5);
    expect(rust).toContain("validate_source_path(path)");
  });

  it("pins every external GitHub Action to a full commit SHA", () => {
    const workflows = filesBelow(".github/workflows")
      .filter((path) => /\.ya?ml$/i.test(path))
      .map(read)
      .join("\n");
    const externalUses = [...workflows.matchAll(/^\s*uses:\s*([^\s#]+).*$/gm)]
      .map((match) => match[1])
      .filter((value) => !value.startsWith("./"));
    expect(externalUses.length).toBeGreaterThan(0);
    for (const action of externalUses) {
      expect(action, `Action 未固定完整 SHA：${action}`).toMatch(
        /^[^@\s]+@[0-9a-f]{40}$/,
      );
    }
    const actionRepositories = [
      ...new Set(externalUses.map((action) => action.split("@")[0])),
    ].sort();
    expect(actionRepositories).toEqual(
      [
        "EmbarkStudios/cargo-deny-action",
        "Swatinem/rust-cache",
        "actions/attest-build-provenance",
        "actions/cache",
        "actions/checkout",
        "actions/download-artifact",
        "actions/setup-node",
        "actions/upload-artifact",
        "dtolnay/rust-toolchain",
        "pnpm/action-setup",
      ].sort(),
    );
  });

  it("verifies tag artifacts read-only before a no-checkout draft publish", () => {
    const workflow = read(".github/workflows/portable-release.yml");
    const verifyStart = workflow.indexOf("  verify-draft:");
    const publishStart = workflow.indexOf("  publish-draft:");
    expect(verifyStart).toBeGreaterThanOrEqual(0);
    expect(publishStart).toBeGreaterThan(verifyStart);
    const verifyJob = workflow.slice(verifyStart, publishStart);
    const publishJob = workflow.slice(publishStart);

    expect(verifyJob).toContain("actions: read");
    expect(verifyJob).toContain("contents: read");
    expect(verifyJob).toContain("attestations: read");
    expect(verifyJob).not.toContain("contents: write");
    expect(verifyJob).toContain("gh run download");
    expect(verifyJob).not.toContain("gh release download");
    expect(verifyJob).toContain("$workflowRunsJson = gh run list");
    expect(verifyJob).toContain("./scripts/select-portable-tag-workflow-run.ps1");
    expect(verifyJob).toContain(
      "$attestationMismatchExitCode = $LASTEXITCODE",
    );
    expect(verifyJob).toContain("$global:LASTEXITCODE = 0");
    expect(publishJob).toContain("contents: write");
    expect(publishJob).not.toContain("actions/checkout@");
    expect(publishJob).toContain("gh release download");
    expect(publishJob).toContain("Get-FileHash");
  });

  it("flattens multi-run GitHub JSON before selecting the immutable tag run", () => {
    if (process.platform !== "win32") return;

    const exact = {
      databaseId: 4,
      headBranch: "v0.4.0",
      headSha: "expected",
      status: "completed",
      conclusion: "success",
    };
    const runs = [
      { ...exact, databaseId: 1, headBranch: "main" },
      { ...exact, databaseId: 2, headSha: "other" },
      { ...exact, databaseId: 3, conclusion: "failure" },
      exact,
    ];
    const select = (input: object[]) =>
      execFileSync(
        "powershell.exe",
        [
          "-NoProfile",
          "-NonInteractive",
          "-ExecutionPolicy",
          "Bypass",
          "-File",
          join(root, "scripts/select-portable-tag-workflow-run.ps1"),
          "-ReleaseTag",
          "v0.4.0",
          "-ExpectedCommit",
          "expected",
        ],
        {
          encoding: "utf8",
          input: JSON.stringify(input),
          stdio: ["pipe", "pipe", "pipe"],
        },
      ).trim();

    expect(select(runs)).toBe("4");
    expect(() => select(runs.slice(0, 3))).toThrow();
    expect(() => select([...runs, { ...exact, databaseId: 5 }])).toThrow();
  });

  it("clears a handled native negative-test exit code before the step ends", () => {
    if (process.platform !== "win32") return;

    const output = execFileSync(
      "powershell.exe",
      [
        "-NoProfile",
        "-NonInteractive",
        "-Command",
        [
          "cmd.exe /c exit 23",
          "$negativeExitCode = $LASTEXITCODE",
          "if ($negativeExitCode -ne 23) { throw 'unexpected exit code' }",
          "$global:LASTEXITCODE = 0",
          "Write-Output 'PASS handled-native-negative-exit'",
        ].join("; "),
      ],
      { encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] },
    );
    expect(output.trim()).toBe("PASS handled-native-negative-exit");
  });

  it("keeps packaged keyboard smoke focused and rapid navigation deterministic", () => {
    const smoke = read("scripts/smoke-native.ps1");
    expect(smoke).not.toContain("[int]($rect.Top + 14)");
    expect(smoke).toContain(
      "[int][Math]::Floor(($rect.Top + $rect.Bottom) / 2)",
    );
    expect(smoke).toContain("function Invoke-ViewerNavigationBurst");
    expect(smoke).toContain(
      "PASS rapid-navigation final=10.jpg trigger=uia-burst count=2 rect=unchanged",
    );
    expect(smoke).toContain("uia-images=[$imageSummary]");
    expect(smoke).toContain("uia-text=[$textSummary]");
  });
});
