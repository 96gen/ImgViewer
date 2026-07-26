import type {
  RenderDescriptor,
  ViewerError,
  ViewerSnapshot,
  ViewerStatus,
} from "../types/viewer";

export const VIEWER_PROTOCOL_VERSION = 1;
export const MAX_RENDER_BYTES = 512 * 1024 * 1024;
const MAX_SIDE = 32_768;
const MAX_PIXELS = 100_000_000;
const MAX_ERROR_MESSAGE_LENGTH = 4_096;
const MAX_FILE_NAME_LENGTH = 1_024;
const MAX_ERROR_PARAMETERS = 32;
const MAX_ERROR_PARAMETER_LENGTH = 1_024;

const STATUSES = new Set<ViewerStatus>(["empty", "loading", "ready", "error"]);
const RENDER_MIME_TYPES = new Set([
  "image/gif",
  "image/jpeg",
  "image/png",
  "image/webp",
]);

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function safeInteger(
  value: unknown,
  field: string,
  minimum = 0,
  maximum = Number.MAX_SAFE_INTEGER,
): number {
  if (
    typeof value !== "number" ||
    !Number.isSafeInteger(value) ||
    value < minimum ||
    value > maximum
  ) {
    throw new Error(`後端協定欄位 ${field} 無效。`);
  }
  return value;
}

function optionalString(
  value: unknown,
  field: string,
  maximumLength: number,
): string | null {
  if (value === null) return null;
  if (typeof value !== "string" || value.length > maximumLength) {
    throw new Error(`後端協定欄位 ${field} 無效。`);
  }
  return value;
}

function parseRender(value: unknown): RenderDescriptor | undefined {
  if (value === undefined) return undefined;
  if (!isRecord(value)) throw new Error("後端 render descriptor 無效。");

  const mimeType = value.mimeType;
  if (typeof mimeType !== "string" || !RENDER_MIME_TYPES.has(mimeType)) {
    throw new Error("後端 render MIME type 不在允許清單。");
  }

  if (typeof value.animated !== "boolean") {
    throw new Error("後端 render animated 欄位無效。");
  }

  const width = safeInteger(value.width, "width", 1, MAX_SIDE);
  const height = safeInteger(value.height, "height", 1, MAX_SIDE);
  if (width * height > MAX_PIXELS) {
    throw new Error("後端 render 總像素超過安全上限。");
  }
  if (
    value.animated &&
    mimeType !== "image/gif" &&
    mimeType !== "image/webp"
  ) {
    throw new Error("後端 render 動畫格式無效。");
  }

  return {
    renderId: safeInteger(value.renderId, "renderId", 1),
    mimeType,
    width,
    height,
    animated: value.animated,
  };
}

function parseErrorParameters(
  value: unknown,
): Record<string, string | number | boolean> {
  if (value === undefined) return {};
  if (!isRecord(value)) throw new Error("後端錯誤參數無效。");

  const entries = Object.entries(value);
  if (entries.length > MAX_ERROR_PARAMETERS) {
    throw new Error("後端錯誤參數數量超過上限。");
  }

  const parameters: Record<string, string | number | boolean> = {};
  for (const [key, item] of entries) {
    const validNumber =
      typeof item === "number" && Number.isSafeInteger(item);
    if (
      !key ||
      key.length > 64 ||
      !(
        (typeof item === "string" &&
          item.length <= MAX_ERROR_PARAMETER_LENGTH) ||
        validNumber ||
        typeof item === "boolean"
      )
    ) {
      throw new Error("後端錯誤參數無效。");
    }
    parameters[key] = item as string | number | boolean;
  }
  return parameters;
}

function parseError(value: unknown): ViewerError | undefined {
  if (value === undefined) return undefined;
  if (!isRecord(value)) throw new Error("後端錯誤 payload 無效。");

  if (
    typeof value.code !== "string" ||
    !/^[a-z0-9][a-z0-9_-]{0,63}$/.test(value.code) ||
    typeof value.message !== "string" ||
    value.message.length > MAX_ERROR_MESSAGE_LENGTH
  ) {
    throw new Error("後端錯誤 payload 無效。");
  }

  return {
    code: value.code,
    message: value.message,
    parameters: parseErrorParameters(value.parameters),
  };
}

export function parseViewerSnapshot(value: unknown): ViewerSnapshot {
  if (!isRecord(value)) throw new Error("後端 snapshot 不是物件。");

  const protocolVersion = safeInteger(
    value.protocolVersion,
    "protocolVersion",
    1,
    65_535,
  );
  if (protocolVersion !== VIEWER_PROTOCOL_VERSION) {
    throw new Error(
      `ImgViewer 前後端協定不相容（收到 ${protocolVersion}，預期 ${VIEWER_PROTOCOL_VERSION}）。`,
    );
  }

  if (typeof value.status !== "string" || !STATUSES.has(value.status as ViewerStatus)) {
    throw new Error("後端 snapshot status 無效。");
  }
  const status = value.status as ViewerStatus;
  const total = safeInteger(value.total, "total");
  const index =
    value.index === null
      ? null
      : safeInteger(value.index, "index", 0, Math.max(0, total - 1));
  if ((total === 0) !== (index === null)) {
    throw new Error("後端 snapshot index/total 不一致。");
  }
  if (typeof value.canPrevious !== "boolean" || typeof value.canNext !== "boolean") {
    throw new Error("後端 snapshot 導航欄位無效。");
  }
  const expectedPrevious = index !== null && index > 0;
  const expectedNext = index !== null && index + 1 < total;
  if (
    value.canPrevious !== expectedPrevious ||
    value.canNext !== expectedNext
  ) {
    throw new Error("後端 snapshot 導航狀態不一致。");
  }

  const render = parseRender(value.render);
  const error = parseError(value.error);
  if (status === "ready" && !render) {
    throw new Error("ready snapshot 缺少 render descriptor。");
  }
  if (status === "error" && !error) {
    throw new Error("error snapshot 缺少錯誤資料。");
  }
  if (status !== "ready" && render) {
    throw new Error("非 ready snapshot 不得包含 render descriptor。");
  }
  if (status !== "error" && error) {
    throw new Error("非 error snapshot 不得包含錯誤資料。");
  }

  const fileName = optionalString(
    value.fileName,
    "fileName",
    MAX_FILE_NAME_LENGTH,
  );
  if ((status === "loading" || status === "ready") && !fileName) {
    throw new Error(`${status} snapshot 缺少檔名。`);
  }

  return {
    protocolVersion,
    revision: safeInteger(value.revision, "revision"),
    generation: safeInteger(value.generation, "generation"),
    status,
    index,
    total,
    fileName,
    canPrevious: value.canPrevious,
    canNext: value.canNext,
    ...(render ? { render } : {}),
    ...(error ? { error } : {}),
  };
}

export function validateRenderBuffer(
  value: unknown,
): ArrayBuffer | ArrayBufferView {
  const valid =
    value instanceof ArrayBuffer ||
    (typeof ArrayBuffer !== "undefined" && ArrayBuffer.isView(value));
  if (!valid) throw new Error("後端未回傳有效的二進位圖片資料。");

  const buffer = value as ArrayBuffer | ArrayBufferView;
  if (buffer.byteLength === 0 || buffer.byteLength > MAX_RENDER_BYTES) {
    throw new Error("後端圖片資料大小無效。");
  }
  return buffer;
}
