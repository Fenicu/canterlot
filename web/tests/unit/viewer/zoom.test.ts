import { describe, expect, it } from "vitest";
import {
  MAX_ZOOM,
  bounds,
  clampView,
  coverRect,
  dismissOpacity,
  dismisses,
  fitScale,
  fitView,
  isZoomed,
  limitScale,
  pinchView,
  rubber,
  rubberView,
  settle,
  swipeStep,
  tapZoomScale,
  toggleZoom,
  transformOf,
  wheelScale,
  zoomAt,
  type Point,
  type View
} from "#lib/viewer/zoom.js";

const phone = { width: 390, height: 800 };
const shot = { width: 1440, height: 900 };

/** Куда на экране попадает точка картинки (u, v) при виде `view`. */
const project = (view: View, u: number, v: number): Point => ({ x: view.x + u * view.scale, y: view.y + v * view.scale });
/** Какая точка картинки лежит под точкой экрана. */
const unproject = (view: View, at: Point): Point => ({ x: (at.x - view.x) / view.scale, y: (at.y - view.y) / view.scale });

describe("viewer/zoom", () => {
  it("fit_puts_the_whole_image_on_the_stage_and_never_upscales_a_small_one", () => {
    expect(fitScale(phone, shot)).toBeCloseTo(390 / 1440);
    expect(fitScale(phone, { width: 200, height: 100 })).toBe(1);
    const view = fitView(phone, shot);
    expect(view.x).toBeCloseTo(0);
    expect(view.y).toBeCloseTo((800 - 900 * (390 / 1440)) / 2);
    expect(isZoomed(view, phone, shot)).toBe(false);
  });

  it("zoom_at_keeps_the_point_under_the_cursor", () => {
    const start = fitView(phone, shot);
    const at = { x: 120, y: 380 };
    const under = unproject(start, at);
    const next = zoomAt(start, at, 1);
    const back = project(next, under.x, under.y);
    expect(back.x).toBeCloseTo(at.x);
    expect(back.y).toBeCloseTo(at.y);
  });

  it("double_tap_zooms_to_natural_pixels_when_two_and_a_half_fit_is_smaller_and_back_to_fit", () => {
    expect(tapZoomScale(phone, shot)).toBe(1);
    // На большом экране 2,5 от вписанного крупнее натурального: берётся 2,5.
    const desk = { width: 1440, height: 800 };
    expect(tapZoomScale(desk, shot)).toBeCloseTo(fitScale(desk, shot) * 2.5);
    const start = fitView(phone, shot);
    const at = { x: 200, y: 400 };
    const under = unproject(start, at);
    const zoomed = toggleZoom(start, at, phone, shot);
    expect(zoomed.scale).toBe(1);
    const back = project(zoomed, under.x, under.y);
    expect(Math.abs(back.x - at.x)).toBeLessThan(1);
    expect(Math.abs(back.y - at.y)).toBeLessThan(1);
    expect(toggleZoom(zoomed, at, phone, shot)).toEqual(fitView(phone, shot));
  });

  it("double_tap_near_an_edge_keeps_the_image_inside_the_bounds", () => {
    const start = fitView(phone, shot);
    const zoomed = toggleZoom(start, { x: 2, y: 2 }, phone, shot);
    const box = bounds(phone, shot, zoomed.scale);
    expect(zoomed.x).toBeLessThanOrEqual(box.maxX);
    expect(zoomed.y).toBeLessThanOrEqual(box.maxY);
    expect(zoomed.x).toBeGreaterThanOrEqual(box.minX);
  });

  it("scale_is_limited_between_fit_and_four_times_natural", () => {
    expect(limitScale(100, phone, shot)).toBe(MAX_ZOOM);
    expect(limitScale(0.01, phone, shot)).toBeCloseTo(fitScale(phone, shot));
    // Щипок за пределами сопротивляется, но не замирает и не уходит дальше запаса.
    const soft = limitScale(8, phone, shot, true);
    expect(soft).toBeGreaterThan(MAX_ZOOM);
    expect(soft).toBeLessThan(MAX_ZOOM * 1.35);
    expect(limitScale(9, phone, shot, true)).toBeGreaterThan(soft);
    expect(limitScale(0.001, phone, shot, true)).toBeGreaterThan(fitScale(phone, shot) * 0.64);
  });

  it("pan_is_bounded_so_the_image_cannot_be_lost", () => {
    const view = clampView({ scale: 1, x: 5000, y: -5000 }, phone, shot);
    expect(view.x).toBe(0);
    expect(view.y).toBe(800 - 900);
    // Узкая по высоте картинка стоит по центру, сдвиг вверх-вниз её не уводит.
    const wide = clampView({ scale: 0.5, x: -100, y: 999 }, phone, shot);
    expect(wide.y).toBeCloseTo((800 - 450) / 2);
  });

  it("rubber_band_pulls_past_the_edge_ever_slower_and_never_further_than_the_span", () => {
    expect(rubber(50, 0, 100, 400)).toBe(50);
    const a = rubber(150, 0, 100, 400);
    const b = rubber(300, 0, 100, 400);
    const c = rubber(100_000, 0, 100, 400);
    expect(a).toBeGreaterThan(100);
    expect(a).toBeLessThan(150);
    expect(b - a).toBeLessThan(150);
    expect(c).toBeLessThan(500);
    expect(rubber(-150, 0, 100, 400)).toBeLessThan(0);
    const pulled = rubberView({ scale: 1, x: 300, y: 0 }, phone, shot);
    expect(pulled.x).toBeGreaterThan(0);
    expect(pulled.x).toBeLessThan(300);
  });

  it("pinch_scales_with_the_finger_distance_around_the_centre", () => {
    const start = fitView(phone, shot);
    const from: [Point, Point] = [{ x: 150, y: 400 }, { x: 250, y: 400 }];
    const to: [Point, Point] = [{ x: 100, y: 400 }, { x: 300, y: 400 }];
    const view = pinchView(start, from, to, phone, shot);
    expect(view.scale).toBeCloseTo(start.scale * 2);
    const under = unproject(start, { x: 200, y: 400 });
    const back = project(view, under.x, under.y);
    expect(back.x).toBeCloseTo(200);
    expect(back.y).toBeCloseTo(400);
  });

  it("pinch_also_pans_with_the_midpoint", () => {
    const start: View = { scale: 1, x: -500, y: -50 };
    const from: [Point, Point] = [{ x: 150, y: 400 }, { x: 250, y: 400 }];
    const to: [Point, Point] = [{ x: 130, y: 380 }, { x: 230, y: 380 }];
    const view = pinchView(start, from, to, phone, shot);
    expect(view.scale).toBeCloseTo(1);
    expect(view.x).toBeCloseTo(-520);
    expect(view.y).toBeCloseTo(-70);
  });

  it("settle_returns_an_overshot_pinch_into_the_limits", () => {
    const tiny = settle({ scale: 0.1, x: 30, y: 30 }, phone, shot);
    expect(tiny).toEqual(fitView(phone, shot));
    const huge = settle({ scale: 6, x: 0, y: 0 }, phone, shot, { x: 0, y: 0 });
    expect(huge.scale).toBe(MAX_ZOOM);
    expect(huge.x).toBe(0);
  });

  it("wheel_zooms_in_on_scroll_up_and_ctrl_pinch_is_finer", () => {
    const view = fitView(phone, shot);
    expect(wheelScale(view, -100, false, phone, shot)).toBeGreaterThan(view.scale);
    expect(wheelScale(view, 100, false, phone, shot)).toBeCloseTo(view.scale);
    const zoomed: View = { scale: 1, x: 0, y: 0 };
    expect(wheelScale(zoomed, 10, true, phone, shot)).toBeLessThan(1);
    expect(wheelScale(zoomed, 10, true, phone, shot)).toBeGreaterThan(wheelScale(zoomed, 100, false, phone, shot) - 0.2);
  });

  it("cover_rect_lays_the_frame_over_a_square_thumbnail_and_clips_the_sides", () => {
    const { view, clip } = coverRect({ x: 10, y: 20, width: 160, height: 160 }, shot);
    expect(view.scale).toBeCloseTo(160 / 900);
    expect(view.y).toBeCloseTo(20);
    const left = project(view, clip.left, 0);
    const right = project(view, shot.width - clip.right, shot.height);
    expect(left.x).toBeCloseTo(10);
    expect(right.x).toBeCloseTo(170);
    expect(clip.top).toBeCloseTo(0);
  });

  it("swipe_and_dismiss_need_a_distance_or_a_fling", () => {
    expect(swipeStep(-40, 0, 390)).toBe(0);
    expect(swipeStep(-120, 0, 390)).toBe(1);
    expect(swipeStep(30, 0.8, 390)).toBe(-1);
    expect(dismisses(60, 0.1, 800)).toBe(false);
    expect(dismisses(160, 0.1, 800)).toBe(true);
    expect(dismisses(40, 1.2, 800)).toBe(true);
    expect(dismissOpacity(0, 800)).toBe(1);
    expect(dismissOpacity(240, 800)).toBeCloseTo(0.5);
    expect(dismissOpacity(9999, 800)).toBe(0);
  });

  it("transform_is_translate_then_scale", () => {
    expect(transformOf({ scale: 2, x: 3, y: -4 })).toBe("translate3d(3px, -4px, 0) scale(2)");
  });
});
