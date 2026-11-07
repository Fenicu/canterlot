import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { describe, expect, it } from 'vitest';
import { OFFICE_INFLATED_MAX_BYTES, inflatedSize, officeProblem } from '#lib/timeline/office/sniff.js';

const fixture = (name: string) => new Uint8Array(readFileSync(join(process.cwd(), 'testing/fixtures/office', name)));

/** Минимальный ZIP из одного пустого файла с заявленным распакованным размером. */
function zipClaiming(size: number): Uint8Array {
  const name = new TextEncoder().encode('a.xml');
  const local = new Uint8Array(30 + name.length);
  const view = new DataView(local.buffer);
  view.setUint32(0, 0x04034b50, true);
  view.setUint16(26, name.length, true);
  local.set(name, 30);
  const head = new Uint8Array(46 + name.length);
  const headView = new DataView(head.buffer);
  headView.setUint32(0, 0x02014b50, true);
  headView.setUint32(24, size, true);
  headView.setUint16(28, name.length, true);
  head.set(name, 46);
  const end = new Uint8Array(22);
  const endView = new DataView(end.buffer);
  endView.setUint32(0, 0x06054b50, true);
  endView.setUint16(8, 1, true);
  endView.setUint16(10, 1, true);
  endView.setUint32(12, head.length, true);
  endView.setUint32(16, local.length, true);
  const out = new Uint8Array(local.length + head.length + end.length);
  out.set(local, 0);
  out.set(head, local.length);
  out.set(end, local.length + head.length);
  return out;
}

describe('timeline/office/sniff', () => {
  it('real_docx_and_xlsx_pass_and_report_their_inflated_size', () => {
    expect(officeProblem(fixture('report.docx'))).toBeNull();
    expect(officeProblem(fixture('table.xlsx'))).toBeNull();
    expect(inflatedSize(fixture('report.docx'))).toBeGreaterThan(fixture('report.docx').length);
  });

  it('legacy_doc_and_password_protected_files_are_told_apart', () => {
    expect(officeProblem(fixture('legacy.doc'))).toBe('legacy');
    expect(officeProblem(fixture('locked.docx'))).toBe('locked');
  });

  it('a_truncated_archive_or_arbitrary_bytes_are_corrupt', () => {
    expect(officeProblem(fixture('broken.docx'))).toBe('corrupt');
    expect(officeProblem(new TextEncoder().encode('<html>not a document</html>'))).toBe('corrupt');
    expect(officeProblem(new Uint8Array())).toBe('corrupt');
  });

  it('an_archive_that_inflates_beyond_the_limit_is_refused_before_parsing', () => {
    expect(officeProblem(zipClaiming(1024))).toBeNull();
    expect(officeProblem(zipClaiming(OFFICE_INFLATED_MAX_BYTES + 1))).toBe('too_large');
    expect(officeProblem(zipClaiming(0xffffffff))).toBe('too_large');
  });
});
