import { describe, expect, it } from "vitest";
import { modelLabel } from "#lib/format/labels.js";

describe("sessions/model label", () => {
  it("model_label_shows_the_1m_variant_by_its_product_name", () => {
    expect(modelLabel("opus[1m]")).toBe("Opus 1M");
    expect(modelLabel("fable[1m]")).toBe("Fable 1M");
  });

  it("model_label_leaves_other_ids_as_they_are", () => {
    expect(modelLabel("opus")).toBe("opus");
    expect(modelLabel("gpt-6.1-sol")).toBe("gpt-6.1-sol");
  });
});
