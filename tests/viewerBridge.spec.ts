import { clearMocks, mockIPC } from "@tauri-apps/api/mocks";
import { afterEach, describe, expect, it, vi } from "vitest";
import { tauriViewerBridge } from "../src/services/viewerBridge";

describe("Tauri viewer bridge", () => {
  afterEach(() => clearMocks());

  it("uses the fixed command names and camelCase binary token argument", async () => {
    const calls: Array<{ command: string; args: unknown }> = [];
    mockIPC((command, args) => {
      calls.push({ command, args });
      if (command === "read_render") return new Uint8Array([1, 2]).buffer;
      return {
        protocolVersion: 1,
        revision: 0,
        generation: 0,
        status: "empty",
        index: null,
        total: 0,
        fileName: null,
        canPrevious: false,
        canNext: false,
      };
    });

    await tauriViewerBridge.openPath("C:\\one.jpg");
    await tauriViewerBridge.navigate("next");
    await tauriViewerBridge.currentSnapshot();
    const bytes = await tauriViewerBridge.readRender(42);

    expect(calls).toEqual([
      { command: "open_path", args: { path: "C:\\one.jpg" } },
      { command: "navigate", args: { direction: "next" } },
      { command: "current_snapshot", args: {} },
      { command: "read_render", args: { renderId: 42 } },
    ]);
    expect(bytes.byteLength).toBe(2);
  });

  it("rejects protocol drift and invalid binary payloads", async () => {
    mockIPC((command) => {
      if (command === "read_render") return new ArrayBuffer(0);
      return {
        protocolVersion: 99,
        revision: 0,
        generation: 0,
        status: "empty",
        index: null,
        total: 0,
        fileName: null,
        canPrevious: false,
        canNext: false,
      };
    });

    await expect(tauriViewerBridge.currentSnapshot()).rejects.toThrow(
      /協定不相容/,
    );
    await expect(tauriViewerBridge.readRender(1)).rejects.toThrow(/大小無效/);
  });
});
