import { afterEach, describe, expect, it } from "vitest";
import { edgeFade } from "#lib/render/edgeFade.js";

function block(width: { scroll: number; client: number }) {
  const host = document.createElement("div");
  const pre = document.createElement("pre");
  host.append(pre);
  document.body.append(host);
  Object.defineProperty(pre, "scrollWidth", { configurable: true, value: width.scroll });
  Object.defineProperty(pre, "clientWidth", { configurable: true, value: width.client });
  return { host, pre };
}

describe("edgeFade", () => {
  afterEach(() => {
    document.body.textContent = "";
  });

  it("marks_a_code_block_that_runs_past_its_edge", () => {
    const { host, pre } = block({ scroll: 900, client: 300 });
    const action = edgeFade(host);
    expect(pre.dataset.fade).toBe("end");
    action.destroy();
  });

  it("leaves_a_block_that_fits_alone", () => {
    const { host, pre } = block({ scroll: 300, client: 300 });
    const action = edgeFade(host);
    expect(pre.dataset.fade).toBeUndefined();
    action.destroy();
  });

  it("follows_the_scroll_to_both_edges_and_back", () => {
    const { host, pre } = block({ scroll: 900, client: 300 });
    const action = edgeFade(host);
    pre.scrollLeft = 200;
    pre.dispatchEvent(new Event("scroll"));
    expect(pre.dataset.fade).toBe("both");
    pre.scrollLeft = 600;
    pre.dispatchEvent(new Event("scroll"));
    expect(pre.dataset.fade).toBe("start");
    action.destroy();
  });
});
