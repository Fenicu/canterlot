#!/usr/bin/env python3
"""Декодирует каждую неосвобождённую запись ownership на диске и сверяет поля
protobuf с message Ownership из нового ipc.proto.

Зеркалит crates/canterlot-worker/src/ownership.rs и canterlot-journal:
- snapshot.toml обязан содержать seq и records (как требует тип Snapshot);
- sealed-файл означает отказ: настоящий opener тоже не открывает такое хранилище;
- записи журнала с seq > snapshot.seq, без пропусков; рваный хвост или битый CRC —
  ошибка (настоящий opener запечатал бы хранилище, и ownership store отказался бы
  его открывать), а не молчаливый обрыв;
- побеждает ПОСЛЕДНЯЯ запись на execution (как HashMap insert);
- проверяется номер И wire type каждого поля, значения — только id и phase.
Печатает только execution id, фазы и номера полей — никогда значения секретов.
Код возврата 1 при проблеме, 2 при отсутствии хранилища/нечитаемом прото.
Только стандартная библиотека.

Явно НЕ проверяется: блокировка каталога (чтение без lock; живой воркер может
дописать сегмент во время обхода — тогда возможен ложный обрыв хвоста);
восстановление карантином (мы только останавливаемся и кричим, не чиним);
содержимое sealed-маркера и заголовка журнала, max_record_bytes и семантика
полезной нагрузки помимо Ownership; CRC самого snapshot.toml.
"""

import os
import re
import struct
import sys
import tomllib
import uuid

RECORD_HEADER = 16
RELEASED_PHASE = 3


_CRC_TABLE = None


def crc32c(data, crc=0):
    global _CRC_TABLE
    if _CRC_TABLE is None:
        poly = 0x82F63B78
        _CRC_TABLE = []
        for i in range(256):
            c = i
            for _ in range(8):
                c = (c >> 1) ^ poly if c & 1 else c >> 1
            _CRC_TABLE.append(c & 0xFFFFFFFF)
    crc ^= 0xFFFFFFFF
    for byte in data:
        crc = _CRC_TABLE[(crc ^ byte) & 0xFF] ^ (crc >> 8)
    return (crc ^ 0xFFFFFFFF) & 0xFFFFFFFF


def checksum(seq, payload):
    return crc32c(payload, crc32c(struct.pack("<Q", seq)))


def read_varint(buf, pos):
    result = 0
    shift = 0
    while True:
        if pos >= len(buf):
            raise ValueError("truncated varint")
        byte = buf[pos]
        pos += 1
        result |= (byte & 0x7F) << shift
        if not byte & 0x80:
            return result, pos
        shift += 7
        if shift >= 64:
            raise ValueError("varint too long")


def walk_fields(payload):
    """{field_number: [(wire_type, value)]}; value int для varint, bytes для LD."""
    fields = {}
    pos = 0
    while pos < len(payload):
        tag, pos = read_varint(payload, pos)
        num, wt = tag >> 3, tag & 7
        if num == 0:
            raise ValueError("field number 0")
        if wt == 0:
            val, pos = read_varint(payload, pos)
        elif wt == 1:
            if pos + 8 > len(payload):
                raise ValueError("truncated fixed64")
            val, pos = None, pos + 8
        elif wt == 2:
            length, pos = read_varint(payload, pos)
            if pos + length > len(payload):
                raise ValueError("truncated length-delimited")
            val = payload[pos:pos + length]
            pos += length
        elif wt == 5:
            if pos + 4 > len(payload):
                raise ValueError("truncated fixed32")
            val, pos = None, pos + 4
        else:
            raise ValueError(f"unsupported wire type {wt} (groups are not used)")
        fields.setdefault(num, []).append((wt, val))
    return fields


VARINT_TYPES = {
    "int32", "int64", "uint32", "uint64", "sint32", "sint64", "bool",
}


def message_fields(proto_path, message):
    """{number: (name, wire_type, integer_kind|None)} для message; wire type
    выводится из типа поля и известных message/enum имён файла, integer_kind —
    диапазон для varint-полей."""
    with open(proto_path, encoding="utf-8") as fh:
        text = fh.read()
    messages = set(re.findall(r"^\s*message\s+(\w+)", text, re.M))
    enums = set(re.findall(r"^\s*enum\s+(\w+)", text, re.M))
    start = text.index(f"message {message} {{")
    depth = 0
    body = []
    for line in text[start:].splitlines():
        depth += line.count("{") - line.count("}")
        body.append(line)
        if depth <= 0 and len(body) > 1:
            break
    out = {}
    for line in body:
        m = re.match(r"\s*(?:repeated\s+|optional\s+)?(\w[\w.]*)\s+(\w+)\s*=\s*(\d+)\s*;",
                     line)
        if not m:
            continue
        tp, name, num = m.group(1), m.group(2), int(m.group(3))
        base = tp.split(".")[-1]
        integer_kind = None
        if base in ("string", "bytes") or base in messages:
            wt = 2
        elif base in ("fixed64", "sfixed64", "double"):
            wt = 1
        elif base in ("fixed32", "sfixed32", "float"):
            wt = 5
        elif base in VARINT_TYPES or base in enums:
            wt = 0
            if base in VARINT_TYPES:
                integer_kind = base
            else:
                integer_kind = "int32"  # enum хранится как i32
        else:
            raise ValueError(f"cannot map proto type '{tp}' to a wire type")
        if num in out:
            raise ValueError(f"duplicate field number {num}")
        out[num] = (name, wt, integer_kind)
    if not out:
        raise ValueError(f"no fields found in message {message}")
    return out


def iter_segments(store_dir):
    """Пути seg-*.log по порядку номеров (имена несут first_seq).

    Как segment::list настоящего opener'а: ровно 20 цифр и seq != 0, иначе
    журнал не открывается.
    """
    try:
        names = sorted(
            n for n in os.listdir(store_dir)
            if n.startswith("seg-") and n.endswith(".log")
        )
    except FileNotFoundError:
        return
    for name in names:
        m = re.fullmatch(r"seg-(\d{20})\.log", name)
        if not m:
            raise ValueError(f"unexpected journal file name: {name}")
        seq = int(m.group(1))
        if seq == 0:
            raise ValueError(f"journal segment starts at seq 0: {name}")
        yield seq, os.path.join(store_dir, name)


def journal_records(store_dir, min_seq):
    """(seq, payload) начиная с max(first_seq, min_seq+1), без пропусков.

    Рваный хвост и несовпадение CRC настоящий opener считает повреждением,
    ставит sealed и ownership store отказывается открывать хранилище; здесь это
    такая же ошибка, а не молчаливый обрыв. Записи до snapshot/min_seq
    пропускаются, но их CRC всё равно проверяется (opener сканирует сегменты
    целиком).
    """
    segments = list(iter_segments(store_dir))
    if not segments:
        return []
    start = max(segments[0][0], min_seq + 1)
    expected = segments[0][0]
    out = []
    for first_seq, path in segments:
        with open(path, "rb") as fh:
            data = fh.read()
        pos = 0
        first_record = True
        while pos < len(data):
            remaining = len(data) - pos
            if remaining < RECORD_HEADER:
                raise ValueError(
                    f"incomplete tail at offset {pos} in {os.path.basename(path)} "
                    f"(opener would seal the store)")
            length, crc, seq = struct.unpack_from("<IIQ", data, pos)
            if length > remaining - RECORD_HEADER:
                raise ValueError(
                    f"incomplete record at seq {seq} in {os.path.basename(path)} "
                    f"(opener would seal the store)")
            payload = data[pos + RECORD_HEADER:pos + RECORD_HEADER + length]
            if crc != checksum(seq, payload):
                raise ValueError(f"checksum mismatch at seq {seq} in {os.path.basename(path)}")
            pos += RECORD_HEADER + length
            if first_record:
                # Имя seg-<first_seq>.log — часть формата: opener проверяет, что
                # первый рекорд сегмента продолжает last_valid_seq, то есть
                # совпадает с first_seq из имени (segment.rs::scan).
                if seq != first_seq:
                    raise ValueError(
                        f"segment {os.path.basename(path)} starts at seq {seq}, "
                        f"name says {first_seq}")
                first_record = False
            if seq != expected:
                raise ValueError(
                    f"sequence gap in journal: got seq {seq}, expected {expected}")
            expected += 1
            if seq >= start:
                out.append((seq, payload))
    return out


def integer_in_range(kind, value):
    """Проверяет диапазон сырого varint для типа Rust/protobuf."""
    if kind == "bool":
        return value in (0, 1)
    if kind == "uint32":
        return 0 <= value <= 2**32 - 1
    if kind == "uint64":
        return 0 <= value <= 2**64 - 1
    if kind == "sint32":
        decoded = (value >> 1) ^ -(value & 1)
        return -(2**31) <= decoded <= 2**31 - 1
    if kind == "sint64":
        decoded = (value >> 1) ^ -(value & 1)
        return -(2**63) <= decoded <= 2**63 - 1
    if kind in ("int32", "int64"):
        # Отрицательные int32/int64 кодируются как 64-битное дополнение.
        decoded = value - 2**64 if value >= 2**63 else value
        if kind == "int32":
            return -(2**31) <= decoded <= 2**31 - 1
        return -(2**63) <= decoded <= 2**63 - 1
    return True


def main():
    if len(sys.argv) != 3:
        print("usage: ownership-check.py <ipc.proto> <worker-data-dir>", file=sys.stderr)
        return 2
    proto_path, data_dir = sys.argv[1], sys.argv[2]
    store_dir = os.path.join(data_dir, "ownership")
    if not os.path.isdir(store_dir):
        print("no ownership store", file=sys.stderr)
        return 2
    # Sealed-маркер: настоящий opener тоже отказывается открывать такое хранилище.
    if os.path.exists(os.path.join(store_dir, "sealed")):
        print("ownership journal is sealed: real opener would refuse it", file=sys.stderr)
        return 1
    try:
        expected = message_fields(proto_path, "Ownership")
    except (ValueError, FileNotFoundError) as exc:
        print(f"cannot parse ipc.proto: {exc}", file=sys.stderr)
        return 2
    snap_path = os.path.join(store_dir, "snapshot.toml")
    try:
        with open(snap_path, "rb") as fh:
            snap = tomllib.load(fh)
    except FileNotFoundError:
        # Как opener: нет файла — пустое хранилище.
        snap = {"seq": 0, "records": []}
    except tomllib.TOMLDecodeError as exc:
        print(f"snapshot.toml parse error: {exc}", file=sys.stderr)
        return 1
    if "seq" not in snap or "records" not in snap:
        print("snapshot.toml lacks required fields (Snapshot requires seq and records)",
              file=sys.stderr)
        return 1
    seq = snap["seq"]
    blobs = snap["records"]
    if not isinstance(seq, int) or isinstance(seq, bool) or not 0 <= seq <= 2**64 - 1:
        print("snapshot.toml seq is not a u64", file=sys.stderr)
        return 1
    if not isinstance(blobs, list):
        print("snapshot.toml has wrong field types", file=sys.stderr)
        return 1
    try:
        journal = journal_records(store_dir, seq)
    except ValueError as exc:
        print(f"journal walk error: {exc}", file=sys.stderr)
        return 1
    blobs = list(blobs) + [payload for _, payload in journal]
    latest = {}
    total = 0
    for blob in blobs:
        total += 1
        if isinstance(blob, (bytes, bytearray)):
            raw = bytes(blob)
        elif isinstance(blob, list) and all(isinstance(b, int) and 0 <= b <= 255 for b in blob):
            raw = bytes(blob)
        else:
            print(f"record {total}: snapshot bytes are not a byte array", file=sys.stderr)
            return 1
        try:
            fields = walk_fields(raw)
        except ValueError as exc:
            print(f"protobuf walk error in record {total}: {exc}", file=sys.stderr)
            return 1
        unknown = sorted(set(fields) - set(expected))
        if unknown:
            print(f"record {total}: unknown field numbers {unknown}", file=sys.stderr)
            return 1
        for num, occurrences in fields.items():
            want = expected[num][1]
            integer_kind = expected[num][2]
            for wt, value in occurrences:
                if wt != want:
                    print(f"record {total}: field {num} ({expected[num][0]}) "
                          f"has wire type {wt}, expected {want}", file=sys.stderr)
                    return 1
                if wt == 0 and integer_kind and not integer_in_range(integer_kind, value):
                    print(f"record {total}: field {num} ({expected[num][0]}) "
                          f"value is outside {integer_kind} range", file=sys.stderr)
                    return 1
        # Последняя запись побеждает (как HashMap insert в opener'е).
        try:
            exec_id = str(uuid.UUID(fields[1][-1][1].decode()))
        except (KeyError, IndexError, UnicodeDecodeError, ValueError, AttributeError) as exc:
            print(f"record {total}: bad execution_id: {exc}", file=sys.stderr)
            return 1
        phase = 0
        if 14 in fields:
            if fields[14][-1][0] != 0:
                print(f"record {total}: phase field is not a varint", file=sys.stderr)
                return 1
            phase = fields[14][-1][1]
        latest[exec_id] = phase
    unreleased = sorted(eid for eid, phase in latest.items() if phase != RELEASED_PHASE)
    print(f"ownership OK: {len(unreleased)} unreleased of {total} records, "
          f"fields {sorted(expected)} known; "
          f"not checked: directory lock, snapshot checksum, sealed/header contents, "
          f"max_record_bytes, payload semantics beyond Ownership")
    for eid in unreleased:
        print(f"UNRELEASED {eid}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
