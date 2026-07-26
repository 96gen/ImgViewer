export type ViewerStatus = "empty" | "loading" | "ready" | "error";
export type NavigationDirection = "previous" | "next";

export interface RenderDescriptor {
  renderId: number;
  mimeType: string;
  width: number;
  height: number;
  animated: boolean;
}

export interface ViewerError {
  code: string;
  message: string;
  parameters: Record<string, string | number | boolean>;
}

export interface ViewerSnapshot {
  protocolVersion: number;
  revision: number;
  generation: number;
  status: ViewerStatus;
  index: number | null;
  total: number;
  fileName: string | null;
  canPrevious: boolean;
  canNext: boolean;
  render?: RenderDescriptor;
  error?: ViewerError;
}
