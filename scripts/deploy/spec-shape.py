#!/usr/bin/env python3
"""Форма живой spec.toml против фикстур и структур ЦЕЛЕВОГО релиза.

Использование: spec-shape.py <supervisor/mod.rs> <config.rs> <spec.toml> <fixture.toml>...
Печатает только имена ключей, никогда значения. Код возврата 0: спека допустима
для цели и совпадает с фикстурой; 1: нет.

Правила (допустимые ключи выводятся из кода цели, а не из ручного списка):
1. Каждый ключ живой спеки обязан быть известен SupervisorSpec цели (тот же разбор
   структур, что у config-check.py, включая открытые карты вроде provider_models).
2. Формы сравниваются с записями открытых карт, свёрнутыми в путь карты
   (`opencode.provider_models.x` -> `opencode.provider_models`).
3. Спека совпадает с фикстурой, если у неё нет ключей, которых нет в фикстуре и которые
   не являются необязательными в цели (Option, коллекция, serde(default) у поля или
   структуры), и в ней есть все ключи фикстуры. Так ключ, добавленный новым релизом
   как необязательный (например opencode.provider_models), не ломает форму.
"""

import importlib.util
import os
import sys
import tomllib

sys.dont_write_bytecode = True  # не оставлять __pycache__ рядом со скриптами
HERE = os.path.dirname(os.path.abspath(__file__))
_spec = importlib.util.spec_from_file_location("config_check", os.path.join(HERE, "config-check.py"))
cc = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(cc)


def load_structs(paths):
    structs = {}
    for path in paths:
        structs.update(cc.parse_structs(path))
    return structs


def optional_field(struct, field_name, structs):
    """Необязательно ли поле в цели: Option, коллекция, serde(default) у поля или структуры."""
    info = structs[struct]
    for name, spec in info["fields"].items():
        if field_name in spec["names"] or field_name == name:
            tp = spec["type"].strip()
            if info.get("default") or tp.startswith(("Option<", "Vec<", "BTreeMap<", "HashMap<")):
                return True
            return spec.get("has_default", False)
    return False


def walk(node, struct, structs, prefix, paths, optional):
    """Собирает свёрнутые пути ключей; открытая карта — один путь, без записей."""
    fields, _flat = cc.resolved_fields(struct, structs)
    for key, value in node.items():
        here = f"{prefix}.{key}" if prefix else key
        paths.add(here)
        if optional_field(struct, key, structs):
            optional.add(here)
        tp = fields.get(key)
        if tp is None or not isinstance(value, dict):
            continue
        kind = cc.classify(tp, structs)
        if kind[0] == "struct":
            walk(value, kind[1], structs, here, paths, optional)
        elif kind[0] == "open_map":
            # записи карты не различаем: путь карты уже учтён
            if kind[1]:
                for sub in value.values():
                    if isinstance(sub, dict):
                        walk(sub, kind[1], structs, here, paths, optional)


def collapse(node, struct, structs, prefix, out):
    """Путь каждого ключа с записями открытых карт, свёрнутыми в путь карты."""
    fields, _flat = cc.resolved_fields(struct, structs)
    for key, value in node.items():
        here = f"{prefix}.{key}" if prefix else key
        out.add(here)
        tp = fields.get(key)
        if tp is None or not isinstance(value, dict):
            continue
        kind = cc.classify(tp, structs)
        if kind[0] == "struct":
            collapse(value, kind[1], structs, here, out)


def shape(path, structs):
    with open(path, "rb") as fh:
        doc = tomllib.load(fh)
    out = set()
    collapse(doc, "SupervisorSpec", structs, "", out)
    return doc, out


def main():
    if len(sys.argv) < 5:
        print("usage: spec-shape.py <supervisor/mod.rs> <config.rs> <spec.toml> <fixture.toml>...", file=sys.stderr)
        return 2
    mod_rs, config_rs, spec_path, fixtures = sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4:]
    structs = load_structs([config_rs, mod_rs])
    if "SupervisorSpec" not in structs:
        print(f"SupervisorSpec not found in {mod_rs}", file=sys.stderr)
        return 2
    doc, live = shape(spec_path, structs)
    errors, warnings = [], []
    cc.check_dict(doc, "SupervisorSpec", structs, "", errors, warnings)
    if errors:
        for err in errors:
            print(f"target does not accept: {err}")
        return 1
    optional = set()
    walk(doc, "SupervisorSpec", structs, "", set(), optional)
    reasons = []
    for fixture in fixtures:
        _fdoc, fkeys = shape(fixture, structs)
        extra = live - fkeys
        required_extra = sorted(extra - optional)
        missing = sorted(fkeys - live)
        if not required_extra and not missing:
            added = sorted(extra)
            note = f" (+ optional in target: {', '.join(added)})" if added else ""
            print(f"MATCH {os.path.basename(fixture)}{note}")
            return 0
        reasons.append(f"{os.path.basename(fixture)}: extra={required_extra} missing={missing}")
    for line in reasons:
        print(f"no fixture shape: {line}")
    return 1


if __name__ == "__main__":
    sys.exit(main())
