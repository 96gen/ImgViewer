import { describe, expect, it } from "vitest";
import {
  MAX_ZOOM,
  MIN_ZOOM,
  clampPan,
  fitScale,
  wheelZoom,
  zoomAroundPoint,
} from "../src/viewer/geometry";

describe("viewer geometry", () => {
  it("fits without cropping and never enlarges a small image", () => {
    expect(fitScale({ width: 2000, height: 1000 }, { width: 1000, height: 700 })).toBe(0.5);
    expect(fitScale({ width: 1000, height: 2000 }, { width: 800, height: 500 })).toBe(0.25);
    expect(fitScale({ width: 1, height: 1 }, { width: 1000, height: 700 })).toBe(1);
  });

  it("keeps the pixel under the pointer fixed while zooming", () => {
    expect(zoomAroundPoint({ x: 0, y: 0 }, { x: 100, y: -50 }, 1, 2)).toEqual({
      x: -100,
      y: 50,
    });
  });

  it("clamps panning to image edges and centers non-overflowing axes", () => {
    expect(
      clampPan(
        { x: 999, y: -999 },
        { width: 1000, height: 400 },
        { width: 500, height: 500 },
        1,
      ),
    ).toEqual({ x: 250, y: 0 });
  });

  it("enforces the 10% to 1600% zoom range", () => {
    expect(wheelZoom(MIN_ZOOM, 10000)).toBe(MIN_ZOOM);
    expect(wheelZoom(MAX_ZOOM, -10000)).toBe(MAX_ZOOM);
  });
});
