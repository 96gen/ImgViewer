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
  });

  it("preserves hard image and allocation limits in the Rust core", () => {
    const rust = filesBelow("src-tauri/src")
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

  it("keeps packaged keyboard smoke away from the native title bar", () => {
    const smoke = read("scripts/smoke-native.ps1");
    expect(smoke).not.toContain("[int]($rect.Top + 14)");
    expect(smoke).toContain(
      "[int][Math]::Floor(($rect.Top + $rect.Bottom) / 2)",
    );
  });
});
