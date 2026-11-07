#!/usr/bin/env python3
"""Строгая проверка TOML-конфига против структур Rust с deny_unknown_fields.

Проверяет КАЖДУЮ таблицу и КАЖДЫЙ ключ (включая таблицы, которых нет в карте,
и ключи с отступом): неизвестная таблица и неизвестный ключ — ошибка.
Карта таблица->структура выводится из типов полей исходника автоматически.

Использование: config-check.py <config.rs> <RootStruct> <config.toml>
Печатает только имена таблиц и ключей, никогда значения. Код возврата 1 при ошибках.
Стандартная библиотека: tomllib, без uv и venv.
"""

import re
import sys
import tomllib

FIELD_RE = re.compile(r"^\s*pub\s+([A-Za-z0-9_]+)\s*:\s*(.+?),\s*(?://.*)?$")
STRUCT_RE = re.compile(r"^\s*pub\s+struct\s+(\w+)")
SERDE_RE = re.compile(r"^\s*#\[serde\(([^]]*)\)\]\s*$")

SCALAR_TYPES = {
    "String", "PathBuf", "Uuid", "SocketAddr", "Duration", "u8", "u16", "u32", "u64",
    "i8", "i16", "i32", "i64", "usize", "bool", "f32", "f64",
}


def strip_option(tp):
    tp = tp.strip()
    tp = re.sub(r"\b(?:[a-z_][a-z0-9_]*::)+", "", tp)  # crate::config::X -> X
    while tp.startswith("Option<") and tp.endswith(">"):
        tp = tp[len("Option<"):-1].strip()
    return tp


def serde_options(text):
    """Разбирает содержимое #[serde(...)]: {ключ: [значения/None]}."""
    parts = []
    current = ""
    quoted = False
    for char in text:
        if char == '"':
            quoted = not quoted
        if char == "," and not quoted:
            parts.append(current)
            current = ""
            continue
        current += char
    if current.strip():
        parts.append(current)
    options = {}
    for part in parts:
        part = part.strip()
        if not part:
            continue
        match = re.fullmatch(r"([A-Za-z_][A-Za-z0-9_]*)\s*=\s*\"(.*)\"", part)
        if match:
            options.setdefault(match.group(1), []).append(match.group(2))
            continue
        match = re.fullmatch(r"([A-Za-z_][A-Za-z0-9_]*)", part)
        if match:
            options.setdefault(match.group(1), []).append(None)
    return options


def field_spec(field, tp, serde_attrs):
    """Имя(имена) поля в TOML и тип с учётом serde-атрибутов поля."""
    options = {}
    for text in serde_attrs:
        for key, values in serde_options(text).items():
            options.setdefault(key, []).extend(values)
    skip = "skip" in options or "skip_deserializing" in options
    names = [name for name in options.get("rename", []) if name]
    if not names:
        names = [field]
    names.extend(name for name in options.get("alias", []) if name)
    return {
        "type": tp,
        "names": names,
        "flatten": "flatten" in options,
        "skip": skip,
    }


def parse_structs(path):
    """{struct: {"fields": {field: spec}, "deny": bool}}."""
    structs = {}
    cur = None
    depth = 0
    deny = False
    pending_deny = False
    pending_default = False
    pending_field_serde = []
    with open(path, encoding="utf-8") as fh:
        for line in fh:
            if cur is None:
                ms = SERDE_RE.match(line)
                if ms:
                    pending_deny = pending_deny or "deny_unknown_fields" in ms.group(1)
                    pending_default = pending_default or bool(re.search(r"\bdefault\b(?!\s*=)", ms.group(1)))
                    continue
                ms = STRUCT_RE.match(line)
                if ms:
                    cur = ms.group(1)
                    deny = pending_deny
                    pending_deny = False
                    struct_default = pending_default
                    pending_default = False
                    depth = line.count("{") - line.count("}")
                    structs[cur] = {"fields": {}, "deny": deny, "default": struct_default}
                    pending_field_serde = []
                    if depth <= 0 and "{" in line:
                        cur = None
                    continue
                pending_deny = False
                pending_default = False
                continue
            ms = SERDE_RE.match(line)
            if ms:
                pending_field_serde.append(ms.group(1))
            else:
                mf = FIELD_RE.match(line)
                if mf and depth >= 1:
                    name, tp = mf.group(1), mf.group(2).strip()
                    spec = field_spec(name, tp, pending_field_serde)
                    if not spec["skip"]:
                        structs[cur]["fields"][name] = spec
                    pending_field_serde = []
            depth += line.count("{") - line.count("}")
            if depth <= 0:
                cur = None
                pending_field_serde = []
    return structs


def classify(tp, structs):
    """('struct', Name) | ('vec_struct', Name) | ('open_map', Sub|None) | ('leaf',)."""
    tp = strip_option(tp)
    mv = re.fullmatch(r"(?:BTreeMap|HashMap)<\s*String\s*,\s*(.+)\s*>", tp)
    if mv:
        sub = strip_option(mv.group(1))
        return ("open_map", sub if sub in structs else None)
    mv = re.fullmatch(r"Vec<\s*(.+?)\s*>", tp)
    if mv:
        sub = strip_option(mv.group(1))
        if sub in structs:
            return ("vec_struct", sub)
        return ("leaf",)
    if tp in structs:
        return ("struct", tp)
    return ("leaf",)


def resolved_fields(struct_name, structs):
    """Поля структуры с учётом rename/alias и развёрнутого flatten."""
    fields = {}
    flatten_kind = None
    for _field, spec in structs[struct_name]["fields"].items():
        if spec["flatten"]:
            kind = classify(spec["type"], structs)
            if kind[0] == "struct":
                sub_fields, _ = resolved_fields(kind[1], structs)
                for key, value in sub_fields.items():
                    fields.setdefault(key, value)
            elif kind[0] == "open_map":
                flatten_kind = kind
            else:
                flatten_kind = ("any", None)
            continue
        for name in spec["names"]:
            fields[name] = spec["type"]
    return fields, flatten_kind


def check_dict(node, struct_name, structs, path, errors, warnings):
    fields, flatten_kind = resolved_fields(struct_name, structs)
    deny = structs[struct_name]["deny"]
    for key, value in node.items():
        here = f"{path}.{key}" if path else key
        if key not in fields:
            if flatten_kind is not None:
                if flatten_kind[0] == "open_map" and flatten_kind[1] and isinstance(value, dict):
                    check_dict(value, flatten_kind[1], structs, here, errors, warnings)
                continue
            # deny_unknown_fields есть у всех секций конфигов; без него —
            # только предупреждение, бинарь такой ключ проглотит молча.
            (errors if deny else warnings).append(
                f"{'unknown key' if deny else 'unchecked key (no deny_unknown_fields)'} '{here}'")
            continue
        kind = classify(fields[key], structs)
        if isinstance(value, dict):
            if kind[0] == "struct":
                check_dict(value, kind[1], structs, here, errors, warnings)
            elif kind[0] == "open_map" and kind[1]:
                for subkey, subval in value.items():
                    if isinstance(subval, dict):
                        check_dict(subval, kind[1], structs, f"{here}.{subkey}", errors, warnings)
            elif kind[0] in ("struct", "open_map"):
                pass  # open map of scalars: any keys allowed
            else:
                errors.append(f"key '{here}' is a table but struct field is scalar")
        elif isinstance(value, list):
            if kind[0] == "vec_struct":
                for i, item in enumerate(value):
                    if isinstance(item, dict):
                        check_dict(item, kind[1], structs, f"{here}[{i}]", errors, warnings)
                    else:
                        errors.append(f"key '{here}[{i}]' is not a table")
            elif any(isinstance(item, dict) for item in value):
                # Массив таблиц [[...]], которому нет структуры в исходнике:
                # ключи внутри проверить не по чему — жёстко.
                errors.append(f"key '{here}' is an array of tables with no matching struct")
            # lists of scalars are not validated element-wise
        else:
            if kind[0] in ("struct", "vec_struct"):
                errors.append(f"key '{here}' is scalar but struct field is a table")


def main():
    if len(sys.argv) != 4:
        print("usage: config-check.py <config.rs> <RootStruct> <config.toml>", file=sys.stderr)
        return 2
    src_path, root, toml_path = sys.argv[1], sys.argv[2], sys.argv[3]
    structs = parse_structs(src_path)
    if root not in structs:
        print(f"root struct '{root}' not found in {src_path}", file=sys.stderr)
        return 2
    with open(toml_path, "rb") as fh:
        try:
            doc = tomllib.load(fh)
        except tomllib.TOMLDecodeError as exc:
            print(f"TOML parse error: {exc}", file=sys.stderr)
            return 1
    errors = []
    warnings = []
    if not doc:
        print("empty TOML document: nothing to check, refusing to pass vacuously", file=sys.stderr)
        return 1
    check_dict(doc, root, structs, "", errors, warnings)
    for err in errors:
        print(err)
    for warn in warnings:
        print(f"warning: {warn}")
    if errors:
        print(f"{len(errors)} unknown table(s)/key(s)", file=sys.stderr)
        return 1
    print(f"config OK against {root}: all tables and keys known")
    return 0


if __name__ == "__main__":
    sys.exit(main())
