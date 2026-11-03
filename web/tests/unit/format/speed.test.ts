import { describe, expect, it } from "vitest";
import { fmt } from "#lib/format/ru.js";

describe("format/speed", () => {
  it("tokens_per_second_rounds_to_whole_and_keeps_tenths_below_ten", () => {
    expect(fmt.tokensPerSecond(142.4)).toBe("142 ток/с");
    expect(fmt.tokensPerSecond(1240)).toBe("1 240 ток/с");
    expect(fmt.tokensPerSecond(4.46)).toBe("4,5 ток/с");
  });

  it("first_token_uses_tenths_of_a_second_then_whole_seconds", () => {
    expect(fmt.firstToken(1800)).toBe("1,8 с");
    expect(fmt.firstToken(420)).toBe("0,4 с");
    expect(fmt.firstToken(20)).toBe("< 0,1 с");
    expect(fmt.firstToken(12_400)).toBe("12 с");
    expect(fmt.firstToken(75_000)).toBe("1 мин 15 с");
  });

  it("speed_line_omits_the_unknown_part", () => {
    expect(fmt.speed(142, 1800)).toBe("генерация 142 ток/с · первый токен 1,8 с");
    expect(fmt.speed(null, 1800)).toBe("первый токен 1,8 с");
    expect(fmt.speed(142, null)).toBe("генерация 142 ток/с");
    expect(fmt.speed(undefined, undefined)).toBe("");
  });
});
