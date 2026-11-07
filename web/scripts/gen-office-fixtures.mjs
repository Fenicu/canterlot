// Небольшие файлы Word и Excel для e2e просмотрщика документов.
// Запуск: node scripts/gen-office-fixtures.mjs — пишет testing/fixtures/office/.
// Архив собирается здесь же из node:zlib: зависимостей у скрипта нет, результат повторяем байт в байт.
import { mkdirSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { crc32, deflateRawSync } from 'node:zlib';

const out = new URL('../testing/fixtures/office/', import.meta.url).pathname;

/** ZIP с фиксированным временем: файлы не меняются от запуска к запуску. */
function zip(entries) {
  const locals = [];
  const central = [];
  let offset = 0;
  for (const [name, content] of entries) {
    const data = typeof content === 'string' ? Buffer.from(content, 'utf8') : content;
    const packed = deflateRawSync(data, { level: 9 });
    const file = Buffer.from(name, 'utf8');
    const crc = crc32(data);
    const local = Buffer.alloc(30);
    local.writeUInt32LE(0x04034b50, 0);
    local.writeUInt16LE(20, 4);
    local.writeUInt16LE(0x0800, 6);
    local.writeUInt16LE(8, 8);
    local.writeUInt16LE(0, 10);
    local.writeUInt16LE(0x5b21, 12);
    local.writeUInt32LE(crc, 14);
    local.writeUInt32LE(packed.length, 18);
    local.writeUInt32LE(data.length, 22);
    local.writeUInt16LE(file.length, 26);
    locals.push(local, file, packed);
    const head = Buffer.alloc(46);
    head.writeUInt32LE(0x02014b50, 0);
    head.writeUInt16LE(20, 4);
    head.writeUInt16LE(20, 6);
    head.writeUInt16LE(0x0800, 8);
    head.writeUInt16LE(8, 10);
    head.writeUInt16LE(0, 12);
    head.writeUInt16LE(0x5b21, 14);
    head.writeUInt32LE(crc, 16);
    head.writeUInt32LE(packed.length, 20);
    head.writeUInt32LE(data.length, 24);
    head.writeUInt16LE(file.length, 28);
    head.writeUInt32LE(offset, 42);
    central.push(head, file);
    offset += local.length + file.length + packed.length;
  }
  const dir = Buffer.concat(central);
  const end = Buffer.alloc(22);
  end.writeUInt32LE(0x06054b50, 0);
  end.writeUInt16LE(entries.length, 8);
  end.writeUInt16LE(entries.length, 10);
  end.writeUInt32LE(dir.length, 12);
  end.writeUInt32LE(offset, 16);
  return Buffer.concat([...locals, dir, end]);
}

/** PNG 160×80: три столбца на светлом поле, без внешних инструментов. */
function png() {
  const width = 160;
  const height = 80;
  const rows = [];
  const bars = [
    { from: 20, to: 50, top: 40, color: [46, 125, 210] },
    { from: 65, to: 95, top: 18, color: [38, 160, 110] },
    { from: 110, to: 140, top: 52, color: [222, 140, 40] }
  ];
  for (let y = 0; y < height; y += 1) {
    const row = Buffer.alloc(1 + width * 3);
    for (let x = 0; x < width; x += 1) {
      const bar = bars.find((item) => x >= item.from && x < item.to && y >= item.top && y < height - 8);
      const [r, g, b] = bar ? bar.color : y === height - 8 ? [120, 120, 120] : [244, 244, 240];
      row.set([r, g, b], 1 + x * 3);
    }
    rows.push(row);
  }
  const chunk = (type, data) => {
    const length = Buffer.alloc(4);
    length.writeUInt32BE(data.length);
    const body = Buffer.concat([Buffer.from(type, 'ascii'), data]);
    const crc = Buffer.alloc(4);
    crc.writeUInt32BE(crc32(body));
    return Buffer.concat([length, body, crc]);
  };
  const header = Buffer.alloc(13);
  header.writeUInt32BE(width, 0);
  header.writeUInt32BE(height, 4);
  header.set([8, 2, 0, 0, 0], 8);
  return Buffer.concat([
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    chunk('IHDR', header),
    chunk('IDAT', deflateRawSyncWrapped(Buffer.concat(rows))),
    chunk('IEND', Buffer.alloc(0))
  ]);
}

/** Поток PNG — zlib с заголовком и Adler-32, а не голый deflate. */
function deflateRawSyncWrapped(data) {
  let a = 1;
  let b = 0;
  for (const byte of data) {
    a = (a + byte) % 65521;
    b = (b + a) % 65521;
  }
  const adler = Buffer.alloc(4);
  adler.writeUInt32BE(((b << 16) | a) >>> 0);
  return Buffer.concat([Buffer.from([0x78, 0xda]), deflateRawSync(data, { level: 9 }), adler]);
}

const escape = (text) => text.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;');
const XML = '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>\n';
const W = 'http://schemas.openxmlformats.org/wordprocessingml/2006/main';
const R = 'http://schemas.openxmlformats.org/officeDocument/2006/relationships';
const PR = 'http://schemas.openxmlformats.org/package/2006/relationships';

function docx() {
  const run = (text, props = '') => `<w:r>${props ? `<w:rPr>${props}</w:rPr>` : ''}<w:t xml:space="preserve">${escape(text)}</w:t></w:r>`;
  const para = (content, style = '', numbering = '') => `<w:p>${style || numbering ? `<w:pPr>${style ? `<w:pStyle w:val="${style}"/>` : ''}${numbering}</w:pPr>` : ''}${content}</w:p>`;
  const item = (text, num) => para(run(text), 'ListParagraph', `<w:numPr><w:ilvl w:val="0"/><w:numId w:val="${num}"/></w:numPr>`);
  const cell = (text, bold = false) => `<w:tc><w:p>${run(text, bold ? '<w:b/>' : '')}</w:p></w:tc>`;
  const row = (cells, bold = false) => `<w:tr>${cells.map((text) => cell(text, bold)).join('')}</w:tr>`;
  const image = `<w:r><w:drawing><wp:inline distT="0" distB="0" distL="0" distR="0"><wp:extent cx="1524000" cy="762000"/><wp:docPr id="1" name="Схема" descr="Схема сборки"/><a:graphic xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/picture"><pic:pic xmlns:pic="http://schemas.openxmlformats.org/drawingml/2006/picture"><pic:nvPicPr><pic:cNvPr id="0" name="image1.png" descr="Схема сборки"/><pic:cNvPicPr/></pic:nvPicPr><pic:blipFill><a:blip r:embed="rId3"/><a:stretch><a:fillRect/></a:stretch></pic:blipFill><pic:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="1524000" cy="762000"/></a:xfrm><a:prstGeom prst="rect"><a:avLst/></a:prstGeom></pic:spPr></pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing></w:r>`;
  const link = (id, text) => `<w:hyperlink r:id="${id}">${run(text, '<w:rStyle w:val="Hyperlink"/>')}</w:hyperlink>`;
  const details = [
    'Сборка запускается на каждом изменении ветки и проверяет веб-клиент, хаб и воркер. Первый этап собирает статический клиент и сверяет размер входного маршрута с бюджетом.',
    'Второй этап гоняет модульные тесты. Они быстрые и не требуют браузера: разметка, форматирование чисел и дат, разбор вложений и очистка HTML проверяются в jsdom.',
    'Третий этап открывает приложение в двух браузерах, на телефоне и на рабочем столе, против поддельного хаба. Любое нарушение политики безопасности содержимого роняет тест.',
    'Отчёт о прогоне прикладывается к сессии файлом. Владелец открывает его прямо в ленте, не скачивая: заголовки, списки и таблицы остаются читаемыми на узком экране.',
    'Если файл повреждён или сохранён в старом формате, просмотрщик честно говорит об этом и предлагает скачать оригинал. Пустой экран вместо документа считается ошибкой.',
    'Картинки из документа показываются через временные адреса blob:, которые освобождаются при закрытии просмотрщика. Внешние картинки по сети не загружаются.'
  ];
  const body = [
    para(run('Отчёт о проверке сборки'), 'Heading1'),
    para(run('Сборка прошла ') + run('успешно', '<w:b/>') + run(', тесты заняли ') + run('четыре минуты', '<w:i/>') + run('.')),
    para(run('Шаги'), 'Heading2'),
    item('Собрать клиент', 1),
    item('Прогнать модульные тесты', 1),
    item('Проверить в браузерах', 1),
    para(run('Порядок выкладки'), 'Heading2'),
    item('Поставить метку версии', 2),
    item('Дождаться зелёной сборки', 2),
    `<w:tbl><w:tblPr><w:tblStyle w:val="TableGrid"/></w:tblPr>${row(['Шаг', 'Время', 'Итог'], true)}${row(['Сборка', '12 с', 'готово'])}${row(['Тесты', '3 мин 40 с', 'готово'])}${row(['Браузеры', '1 мин 5 с', 'готово'])}</w:tbl>`,
    para(image),
    para(run('Подробности: ') + link('rId4', 'Документация')),
    para(link('rId5', 'Опасная ссылка')),
    para(run('Как устроена проверка'), 'Heading2'),
    ...details.map((text) => para(run(text)))
  ].join('');
  const document = `${XML}<w:document xmlns:w="${W}" xmlns:r="${R}" xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing"><w:body>${body}<w:sectPr/></w:body></w:document>`;
  const style = (id, name, type = 'paragraph') => `<w:style w:type="${type}" w:styleId="${id}"><w:name w:val="${name}"/></w:style>`;
  const styles = `${XML}<w:styles xmlns:w="${W}">${style('Normal', 'Normal')}${style('Heading1', 'heading 1')}${style('Heading2', 'heading 2')}${style('ListParagraph', 'List Paragraph')}${style('Hyperlink', 'Hyperlink', 'character')}${style('TableGrid', 'Table Grid', 'table')}</w:styles>`;
  const level = (format, text) => `<w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="${format}"/><w:lvlText w:val="${text}"/></w:lvl>`;
  const numbering = `${XML}<w:numbering xmlns:w="${W}"><w:abstractNum w:abstractNumId="0">${level('bullet', '•')}</w:abstractNum><w:abstractNum w:abstractNumId="1">${level('decimal', '%1.')}</w:abstractNum><w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num><w:num w:numId="2"><w:abstractNumId w:val="1"/></w:num></w:numbering>`;
  return zip([
    ['[Content_Types].xml', `${XML}<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Default Extension="png" ContentType="image/png"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml"/><Override PartName="/word/numbering.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml"/></Types>`],
    ['_rels/.rels', `${XML}<Relationships xmlns="${PR}"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>`],
    ['word/document.xml', document],
    ['word/_rels/document.xml.rels', `${XML}<Relationships xmlns="${PR}"><Relationship Id="rId1" Type="${R}/styles" Target="styles.xml"/><Relationship Id="rId2" Type="${R}/numbering" Target="numbering.xml"/><Relationship Id="rId3" Type="${R}/image" Target="media/image1.png"/><Relationship Id="rId4" Type="${R}/hyperlink" Target="https://example.com/docs" TargetMode="External"/><Relationship Id="rId5" Type="${R}/hyperlink" Target="javascript:alert(1)" TargetMode="External"/></Relationships>`],
    ['word/styles.xml', styles],
    ['word/numbering.xml', numbering],
    ['word/media/image1.png', png()]
  ]);
}

/** Порядковый номер дня в Excel (система 1900) для даты UTC. */
const serial = (iso) => Date.parse(`${iso}Z`) / 86_400_000 + 25_569;
const column = (index) => {
  let name = '';
  for (let n = index + 1; n > 0; n = Math.floor((n - 1) / 26)) name = String.fromCharCode(65 + ((n - 1) % 26)) + name;
  return name;
};

function xlsx() {
  const strings = [];
  const lookup = new Map();
  const text = (value) => {
    if (!lookup.has(value)) {
      lookup.set(value, strings.length);
      strings.push(value);
    }
    return lookup.get(value);
  };
  // Ячейка: строка, число, дата ({ date }), формула с сохранённым результатом ({ f, v, s }) или логическое значение.
  const cell = (ref, value) => {
    if (value === null || value === undefined) return '';
    if (typeof value === 'string') return `<c r="${ref}" t="s"><v>${text(value)}</v></c>`;
    if (typeof value === 'number') return `<c r="${ref}"><v>${value}</v></c>`;
    if (typeof value === 'boolean') return `<c r="${ref}" t="b"><v>${value ? 1 : 0}</v></c>`;
    if ('date' in value) return `<c r="${ref}" s="${value.time ? 2 : 1}"><v>${serial(value.date)}</v></c>`;
    return `<c r="${ref}"${value.s ? ` s="${value.s}"` : ''}><f>${escape(value.f)}</f><v>${value.v}</v></c>`;
  };
  const sheet = (rows) => {
    const width = Math.max(...rows.map((row) => row.length));
    const data = rows.map((row, r) => `<row r="${r + 1}">${row.map((value, c) => cell(`${column(c)}${r + 1}`, value)).join('')}</row>`).join('');
    return `${XML}<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="${R}"><dimension ref="A1:${column(width - 1)}${rows.length}"/><sheetData>${data}</sheetData></worksheet>`;
  };

  const summary = [
    ['Показатель', 'Значение', 'Дата', 'Готово'],
    ['Выручка', 1_250_000.5, { date: '2026-10-01T00:00:00' }, true],
    ['Расходы', 830_000, { date: '2026-10-15T00:00:00' }, false],
    ['Прибыль', { f: 'B2-B3', v: 420_000.5 }, { date: '2026-10-31T18:30:00', time: true }, true],
    ['Доля прибыли', { f: 'B4/B2', v: 0.3359997, s: 3 }, null, null]
  ];
  const people = ['Анна', 'Борис', 'Вера', 'Глеб', 'Дина'];
  const states = ['готово', 'в работе', 'ждёт проверки'];
  const head = ['Дата', 'Номер', 'Задача', 'Исполнитель', 'Статус', 'Часы', 'Ставка', 'Сумма', 'Проект', 'Этап', 'Приоритет', 'Отдел', 'Город', 'Комментарий'];
  const log = [head];
  for (let i = 1; i <= 2200; i += 1) {
    const r = i + 1;
    const hours = (i % 9) + 0.5;
    const rate = 1500 + (i % 4) * 250;
    const day = new Date(Date.UTC(2026, 0, 1 + (i % 300))).toISOString().slice(0, 10);
    log.push([
      { date: `${day}T00:00:00` },
      i,
      `Задача ${i}`,
      people[i % people.length],
      states[i % states.length],
      hours,
      rate,
      { f: `F${r}*G${r}`, v: hours * rate },
      `Проект ${(i % 7) + 1}`,
      `Этап ${(i % 3) + 1}`,
      ['низкий', 'средний', 'высокий'][i % 3],
      ['Разработка', 'Поддержка', 'Продажи'][i % 3],
      ['Москва', 'Казань', 'Пермь', 'Томск'][i % 4],
      i % 5 === 0 ? 'Нужна проверка владельца перед выкладкой' : ''
    ]);
  }
  const sheets = [sheet(summary), sheet(log)];
  const shared = `${XML}<sst xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" count="${strings.length}" uniqueCount="${strings.length}">${strings.map((value) => `<si><t xml:space="preserve">${escape(value)}</t></si>`).join('')}</sst>`;
  // 0 — обычный, 1 — дата (14), 2 — дата и время (22), 3 — проценты (10).
  const styles = `${XML}<styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><fonts count="1"><font><sz val="11"/><name val="Calibri"/></font></fonts><fills count="2"><fill><patternFill patternType="none"/></fill><fill><patternFill patternType="gray125"/></fill></fills><borders count="1"><border/></borders><cellStyleXfs count="1"><xf numFmtId="0" fontId="0" fillId="0" borderId="0"/></cellStyleXfs><cellXfs count="4"><xf numFmtId="0" fontId="0" fillId="0" borderId="0" xfId="0"/><xf numFmtId="14" fontId="0" fillId="0" borderId="0" xfId="0" applyNumberFormat="1"/><xf numFmtId="22" fontId="0" fillId="0" borderId="0" xfId="0" applyNumberFormat="1"/><xf numFmtId="10" fontId="0" fillId="0" borderId="0" xfId="0" applyNumberFormat="1"/></cellXfs></styleSheet>`;
  const S = 'http://schemas.openxmlformats.org/officeDocument/2006/relationships';
  return zip([
    ['[Content_Types].xml', `${XML}<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/><Override PartName="/xl/worksheets/sheet2.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/><Override PartName="/xl/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.styles+xml"/><Override PartName="/xl/sharedStrings.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sharedStrings+xml"/></Types>`],
    ['_rels/.rels', `${XML}<Relationships xmlns="${PR}"><Relationship Id="rId1" Type="${S}/officeDocument" Target="xl/workbook.xml"/></Relationships>`],
    ['xl/workbook.xml', `${XML}<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="${R}"><sheets><sheet name="Сводка" sheetId="1" r:id="rId1"/><sheet name="Журнал" sheetId="2" r:id="rId2"/></sheets></workbook>`],
    ['xl/_rels/workbook.xml.rels', `${XML}<Relationships xmlns="${PR}"><Relationship Id="rId1" Type="${S}/worksheet" Target="worksheets/sheet1.xml"/><Relationship Id="rId2" Type="${S}/worksheet" Target="worksheets/sheet2.xml"/><Relationship Id="rId3" Type="${S}/styles" Target="styles.xml"/><Relationship Id="rId4" Type="${S}/sharedStrings" Target="sharedStrings.xml"/></Relationships>`],
    ['xl/worksheets/sheet1.xml', sheets[0]],
    ['xl/worksheets/sheet2.xml', sheets[1]],
    ['xl/styles.xml', styles],
    ['xl/sharedStrings.xml', shared]
  ]);
}

/** Составной файл OLE2: так начинаются старые .doc/.xls и защищённые паролем .docx/.xlsx. */
function compound(marker = '') {
  const file = Buffer.alloc(1536);
  file.set([0xd0, 0xcf, 0x11, 0xe0, 0xa1, 0xb1, 0x1a, 0xe1], 0);
  if (marker) file.write(marker, 512, 'utf16le');
  return file;
}

mkdirSync(out, { recursive: true });
const report = docx();
const files = {
  'report.docx': report,
  'table.xlsx': xlsx(),
  // Обрезанный архив: начало верное, центрального каталога нет.
  'broken.docx': report.subarray(0, 400),
  'legacy.doc': compound(),
  'locked.docx': compound('EncryptedPackage')
};
for (const [name, data] of Object.entries(files)) {
  writeFileSync(join(out, name), data);
  process.stdout.write(`${name}: ${data.length} байт\n`);
}
