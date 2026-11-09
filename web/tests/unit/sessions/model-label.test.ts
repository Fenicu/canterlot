import { describe, expect, it } from "vitest";
import { modelAliasNote, modelLabel, resolvedModelLabel } from "#lib/format/labels.js";

describe("sessions/model label", () => {
  it("model_label_shows_the_1m_variant_by_its_product_name", () => {
    expect(modelLabel("opus[1m]")).toBe("Opus 1M");
    expect(modelLabel("fable[1m]")).toBe("Fable 1M");
    expect(modelLabel("claude-opus-5-5[1m]")).toBe("Opus 5.5 1M");
  });

  it("model_label_names_concrete_claude_versions", () => {
    expect(modelLabel("claude-fable-5-2")).toBe("Fable 5.2");
    expect(modelLabel("claude-opus-5-5")).toBe("Opus 5.5");
    expect(modelLabel("claude-sonnet-4-5-20250929")).toBe("Sonnet 4.5");
    // Незнакомое семейство: название с заглавной и версия через точку.
    expect(modelLabel("claude-quill-7-1")).toBe("Quill 7.1");
  });

  it("model_label_names_codex_models", () => {
    expect(modelLabel("gpt-6.1-sol")).toBe("GPT-6.1 Sol");
    expect(modelLabel("gpt-6-luna")).toBe("GPT-6 Luna");
  });

  it("model_label_hides_the_provider_of_a_qualified_id", () => {
    expect(modelLabel("opencode-go/muse-spark-1.3-contributor")).toBe(
      "muse-spark-1.3-contributor",
    );
    expect(modelLabel("openrouter/anthropic/fable")).toBe("anthropic/fable");
  });

  it("model_label_capitalizes_a_bare_alias", () => {
    expect(modelLabel("opus")).toBe("Opus");
    expect(modelLabel("synthetic-large")).toBe("synthetic-large");
  });

  it("resolved_label_prefers_the_concrete_version_and_keeps_1m", () => {
    expect(resolvedModelLabel("fable[1m]", "claude-fable-5-2")).toBe("Fable 5.2 1M");
    expect(resolvedModelLabel("sonnet", "claude-sonnet-5-5")).toBe("Sonnet 5.5");
    expect(resolvedModelLabel("fable[1m]", null)).toBe("Fable 1M");
    expect(resolvedModelLabel("gpt-6-luna", "gpt-6-luna")).toBe("GPT-6 Luna");
  });

  it("alias_note_maps_alias_to_id_only_for_aliases", () => {
    expect(modelAliasNote("fable[1m]", "claude-fable-5-2")).toBe("fable[1m] → claude-fable-5-2");
    expect(modelAliasNote("gpt-6-luna", "gpt-6-luna")).toBe("");
    expect(modelAliasNote("opus", null)).toBe("");
  });
});
