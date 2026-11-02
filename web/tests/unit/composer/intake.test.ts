import { describe, expect, it } from "vitest";
import {
  filesFromPayload,
  hasFileDrag,
  readableFileName,
} from "#lib/composer/intake.js";

const AT = new Date(2026, 10, 2, 18, 10);

function item(file: File | null) {
  return { kind: "file", getAsFile: () => file };
}

function textItem() {
  return { kind: "string", getAsFile: () => null };
}

function payload(files: File[] = [], items: ReturnType<typeof item | typeof textItem>[] = []) {
  return { files, items };
}

function file(name: string, type = "text/plain", body = "x") {
  return new File([body], name, { type, lastModified: 0 });
}

describe("composer/intake", () => {
  it("text_only_payload_yields_no_files", () => {
    expect(filesFromPayload(payload([], [textItem()]), AT)).toEqual([]);
    expect(filesFromPayload(payload(), AT)).toEqual([]);
    expect(filesFromPayload(null, AT)).toEqual([]);
  });

  it("files_only_payload_keeps_every_file_in_order", () => {
    const first = file("a.txt");
    const second = file("b.txt");
    expect(filesFromPayload(payload([first, second]), AT)).toEqual([first, second]);
  });

  it("several_file_items_are_all_taken_and_text_items_are_skipped", () => {
    const first = file("a.txt");
    const second = file("b.txt");
    const found = filesFromPayload(
      payload([], [textItem(), item(first), textItem(), item(second)]),
      AT,
    );
    expect(found).toEqual([first, second]);
  });

  it("files_and_matching_items_do_not_add_the_same_file_twice", () => {
    const original = file("same.txt");
    const copy = new File([original], original.name, {
      type: original.type,
      lastModified: original.lastModified,
    });
    expect(
      filesFromPayload(payload([original], [item(copy)]), AT),
    ).toHaveLength(1);
  });

  it("files_that_items_do_not_contain_are_still_added", () => {
    const fromItems = file("from-items.txt");
    const onlyFile = file("only-file.txt");
    const found = filesFromPayload(payload([onlyFile], [item(fromItems)]), AT);
    expect(found).toEqual([fromItems, onlyFile]);
  });

  it("identical_files_inside_one_source_are_all_kept", () => {
    const first = file("same.txt", "text/plain", "x");
    const second = new File([first], first.name, {
      type: first.type,
      lastModified: first.lastModified,
    });
    expect(filesFromPayload(payload([first, second]), AT)).toHaveLength(2);
  });

  it("unnamed_images_get_a_readable_local_name_and_keep_their_type", () => {
    const shot = new File(["png"], "", { type: "image/png" });
    const found = filesFromPayload(payload([shot]), AT);
    expect(found).toHaveLength(1);
    expect(found[0]?.name).toBe("Снимок экрана 2026-11-02 18-10.png");
    expect(found[0]?.type).toBe("image/png");
    expect(readableFileName(new File(["jpg"], "image.jpeg", { type: "image/jpeg" }), AT).name).toBe(
      "Снимок экрана 2026-11-02 18-10.jpg",
    );
    expect(readableFileName(shot, AT).name).toBe("Снимок экрана 2026-11-02 18-10.png");
  });

  it("named_files_and_non_empty_images_keep_their_names", () => {
    const namedImage = file("кот.png", "image/png");
    const document = file("заметки.txt");
    expect(readableFileName(namedImage, AT)).toBe(namedImage);
    expect(readableFileName(document, AT)).toBe(document);
    expect(filesFromPayload(payload([namedImage, document]), AT)).toEqual([
      namedImage,
      document,
    ]);
  });

  it("file_drag_is_recognised_by_types_or_files", () => {
    expect(hasFileDrag({ types: ["Files"], files: [] })).toBe(true);
    expect(hasFileDrag({ types: ["text/plain"], files: [] })).toBe(false);
    expect(hasFileDrag({ types: [], files: [file("a.txt")] })).toBe(true);
    expect(hasFileDrag(null)).toBe(false);
  });
});
