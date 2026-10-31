import { describe, expect, it } from "vitest";
import { menuTop } from "#lib/screens/sessions/menuPlace.js";

describe("session menu placement", () => {
  it("sits_under_the_row_when_it_fits", () => {
    expect(menuTop({ full: 300, height: 800, rowTop: 100, rowBottom: 180 })).toBe(180);
  });

  it("goes_above_the_row_when_there_is_no_room_below", () => {
    expect(menuTop({ full: 300, height: 800, rowTop: 650, rowBottom: 730 })).toBe(350);
  });

  it("covers_the_row_at_the_bottom_edge_when_it_fits_on_neither_side", () => {
    expect(menuTop({ full: 500, height: 600, rowTop: 200, rowBottom: 280 })).toBe(100);
  });

  it("starts_at_the_top_when_it_is_taller_than_the_list", () => {
    expect(menuTop({ full: 900, height: 600, rowTop: 200, rowBottom: 280 })).toBe(0);
  });
});
