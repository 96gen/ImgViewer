export interface Size {
  width: number;
  height: number;
}

export interface Point {
  x: number;
  y: number;
}

export const MIN_ZOOM = 0.1;
export const MAX_ZOOM = 16;

export function clampZoom(scale: number): number {
  return Math.min(MAX_ZOOM, Math.max(MIN_ZOOM, scale));
}

export function fitScale(image: Size, viewport: Size): number {
  if (
    image.width <= 0 ||
    image.height <= 0 ||
    viewport.width <= 0 ||
    viewport.height <= 0
  ) {
    return 1;
  }

  return Math.min(
    1,
    viewport.width / image.width,
    viewport.height / image.height,
  );
}

export function clampPan(
  offset: Point,
  image: Size,
  viewport: Size,
  scale: number,
): Point {
  const overflowX = Math.max(0, (image.width * scale - viewport.width) / 2);
  const overflowY = Math.max(0, (image.height * scale - viewport.height) / 2);

  return {
    x: overflowX === 0 ? 0 : Math.min(overflowX, Math.max(-overflowX, offset.x)),
    y: overflowY === 0 ? 0 : Math.min(overflowY, Math.max(-overflowY, offset.y)),
  };
}

export function zoomAroundPoint(
  offset: Point,
  anchor: Point,
  oldScale: number,
  newScale: number,
): Point {
  if (oldScale <= 0) return offset;
  const ratio = newScale / oldScale;
  return {
    x: anchor.x - (anchor.x - offset.x) * ratio,
    y: anchor.y - (anchor.y - offset.y) * ratio,
  };
}

export function wheelZoom(scale: number, deltaY: number): number {
  const factor = Math.exp(-deltaY * 0.0015);
  return clampZoom(scale * factor);
}
