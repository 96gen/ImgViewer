import { invoke } from "@tauri-apps/api/core";
import type { UnlistenFn } from "@tauri-apps/api/event";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import { open } from "@tauri-apps/plugin-dialog";
import type {
  NavigationDirection,
  ViewerSnapshot,
} from "../types/viewer";
import {
  parseViewerSnapshot,
  validateRenderBuffer,
} from "./snapshotProtocol";

export type SnapshotListener = (snapshot: ViewerSnapshot) => void;
export type ProtocolErrorListener = (error: Error) => void;
export type FileDropListener = (path: string) => void;

export interface ViewerBridge {
  chooseImage(): Promise<string | null>;
  openPath(path: string): Promise<ViewerSnapshot>;
  navigate(direction: NavigationDirection): Promise<ViewerSnapshot>;
  currentSnapshot(): Promise<ViewerSnapshot>;
  readRender(renderId: number): Promise<ArrayBuffer | ArrayBufferView>;
  listenSnapshot(
    listener: SnapshotListener,
    onProtocolError?: ProtocolErrorListener,
  ): Promise<UnlistenFn>;
  listenFileDrop(listener: FileDropListener): Promise<UnlistenFn>;
}

const IMAGE_FILTER = {
  name: "支援的圖片",
  extensions: ["gif", "jpg", "jpeg", "png", "tif", "tiff", "webp", "heic", "heif"],
};

export const tauriViewerBridge: ViewerBridge = {
  async chooseImage() {
    const result = await open({
      multiple: false,
      directory: false,
      filters: [IMAGE_FILTER],
    });

    return typeof result === "string" ? result : null;
  },

  async openPath(path) {
    return parseViewerSnapshot(await invoke<unknown>("open_path", { path }));
  },

  async navigate(direction) {
    return parseViewerSnapshot(await invoke<unknown>("navigate", { direction }));
  },

  async currentSnapshot() {
    return parseViewerSnapshot(await invoke<unknown>("current_snapshot"));
  },

  async readRender(renderId) {
    return validateRenderBuffer(await invoke<unknown>("read_render", { renderId }));
  },

  listenSnapshot(listener, onProtocolError) {
    return getCurrentWebviewWindow().listen<unknown>(
      "viewer://snapshot",
      (event) => {
        try {
          listener(parseViewerSnapshot(event.payload));
        } catch (error) {
          onProtocolError?.(
            error instanceof Error ? error : new Error("後端 snapshot 協定無效。"),
          );
        }
      },
    );
  },

  listenFileDrop(listener) {
    return getCurrentWebviewWindow().onDragDropEvent((event) => {
      if (event.payload.type !== "drop") return;
      const firstPath = event.payload.paths[0];
      if (firstPath) listener(firstPath);
    });
  },
};
