import { invoke } from "@tauri-apps/api/core";
import type { UnlistenFn } from "@tauri-apps/api/event";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import { open } from "@tauri-apps/plugin-dialog";
import type {
  NavigationDirection,
  ViewerSnapshot,
} from "../types/viewer";

export type SnapshotListener = (snapshot: ViewerSnapshot) => void;
export type FileDropListener = (path: string) => void;

export interface ViewerBridge {
  chooseImage(): Promise<string | null>;
  openPath(path: string): Promise<ViewerSnapshot>;
  navigate(direction: NavigationDirection): Promise<ViewerSnapshot>;
  currentSnapshot(): Promise<ViewerSnapshot>;
  readRender(renderId: number): Promise<ArrayBuffer | ArrayBufferView>;
  listenSnapshot(listener: SnapshotListener): Promise<UnlistenFn>;
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

  openPath(path) {
    return invoke<ViewerSnapshot>("open_path", { path });
  },

  navigate(direction) {
    return invoke<ViewerSnapshot>("navigate", { direction });
  },

  currentSnapshot() {
    return invoke<ViewerSnapshot>("current_snapshot");
  },

  readRender(renderId) {
    return invoke<ArrayBuffer>("read_render", { renderId });
  },

  listenSnapshot(listener) {
    return getCurrentWebviewWindow().listen<ViewerSnapshot>(
      "viewer://snapshot",
      (event) => listener(event.payload),
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
