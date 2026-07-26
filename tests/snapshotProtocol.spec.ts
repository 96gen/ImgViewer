import { describe, expect, it } from "vitest";
import {
  MAX_RENDER_BYTES,
  parseViewerSnapshot,
  validateRenderBuffer,
  VIEWER_PROTOCOL_VERSION,
} from "../src/services/snapshotProtocol";

const readyPayload = () => ({
  protocolVersion: VIEWER_PROTOCOL_VERSION,
  revision: 12,
  generation: 6,
  status: "ready",
  index: 1,
  total: 3,
  fileName: "safe.png",
  canPrevious: true,
  canNext: true,
  render: {
    renderId: 42,
    mimeType: "image/png",
    width: 640,
    height: 480,
    animated: false,
  },
});

describe("viewer snapshot protocol", () => {
  it("accepts a bounded version-1 ready snapshot", () => {
    expect(parseViewerSnapshot(readyPayload())).toEqual(readyPayload());
  });

  it("fails closed on protocol drift and malformed state combinations", () => {
    expect(() =>
      parseViewerSnapshot({ ...readyPayload(), protocolVersion: 2 }),
    ).toThrow(/協定不相容/);
    expect(() =>
      parseViewerSnapshot({ ...readyPayload(), status: "loading" }),
    ).toThrow(/非 ready snapshot/);
    expect(() =>
      parseViewerSnapshot({ ...readyPayload(), render: undefined }),
    ).toThrow(/缺少 render/);
    expect(() =>
      parseViewerSnapshot({
        ...readyPayload(),
        render: { ...readyPayload().render, mimeType: "text/html" },
      }),
    ).toThrow(/MIME type/);
  });

  it("rejects invalid navigation, dimensions, names, and error parameters", () => {
    expect(() =>
      parseViewerSnapshot({ ...readyPayload(), index: 3 }),
    ).toThrow(/index/);
    expect(() =>
      parseViewerSnapshot({ ...readyPayload(), canNext: false }),
    ).toThrow(/導航狀態/);
    expect(() =>
      parseViewerSnapshot({
        ...readyPayload(),
        render: { ...readyPayload().render, width: 32_769 },
      }),
    ).toThrow(/width/);
    expect(() =>
      parseViewerSnapshot({
        ...readyPayload(),
        render: {
          ...readyPayload().render,
          width: 20_000,
          height: 20_000,
        },
      }),
    ).toThrow(/總像素/);
    expect(() =>
      parseViewerSnapshot({
        ...readyPayload(),
        render: { ...readyPayload().render, animated: true },
      }),
    ).toThrow(/動畫格式/);
    expect(() =>
      parseViewerSnapshot({ ...readyPayload(), fileName: "x".repeat(1_025) }),
    ).toThrow(/fileName/);
    expect(() =>
      parseViewerSnapshot({
        ...readyPayload(),
        status: "error",
        render: undefined,
        error: {
          code: "bad code",
          message: "bad",
          parameters: {},
        },
      }),
    ).toThrow(/錯誤 payload/);
  });

  it("accepts bounded structured error parameters emitted by Rust", () => {
    const parsed = parseViewerSnapshot({
      ...readyPayload(),
      status: "error",
      render: undefined,
      error: {
        code: "decode_deadline_exceeded",
        message: "圖片解碼超過安全期限。",
        parameters: {
          limitMs: 30_000,
          recoverable: true,
          phase: "decode",
        },
      },
    });

    expect(parsed.error?.parameters).toEqual({
      limitMs: 30_000,
      recoverable: true,
      phase: "decode",
    });
  });

  it("accepts ArrayBuffer views but rejects empty, oversized, and non-binary data", () => {
    const bytes = new Uint8Array([1, 2, 3]);
    expect(validateRenderBuffer(bytes)).toBe(bytes);
    expect(() => validateRenderBuffer(new ArrayBuffer(0))).toThrow(/大小無效/);
    expect(() => validateRenderBuffer({ byteLength: 3 })).toThrow(/二進位/);

    const oversized = new Uint8Array([1]);
    Object.defineProperty(oversized, "byteLength", {
      value: MAX_RENDER_BYTES + 1,
    });
    expect(() => validateRenderBuffer(oversized)).toThrow(/大小無效/);
  });
});
