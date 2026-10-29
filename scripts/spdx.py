"""SPDX license expression tokenizer, parser, and allowlist evaluator."""

import re

ALLOWLIST = {
    'MIT', 'Apache-2.0', 'BSD-2-Clause', 'BSD-3-Clause', 'ISC', 'Zlib',
    'Unicode-3.0', 'Unicode-DFS-2016', 'MPL-2.0', 'CC0-1.0', '0BSD',
    'BSL-1.0', 'Unlicense', 'MIT-0'
}


def preference_score(chosen_str):
    """Rank chosen alternatives: MIT (0) > Apache-2.0 (1) > others (2)."""
    if not chosen_str:
        return 999
    tokens = re.findall(r'[A-Za-z0-9_.\-+:]+', chosen_str)
    if 'MIT' in tokens:
        return 0
    if 'Apache-2.0' in tokens:
        return 1
    return 2


class LicenseNode:
    def __init__(self, ident):
        self.ident = ident

    def evaluate(self, allowlist):
        if self.ident in allowlist:
            return True, self.ident, []
        return False, None, [self.ident]


class WithNode:
    def __init__(self, license_node, exception_ident):
        self.license_node = license_node
        self.exception_ident = exception_ident

    def evaluate(self, allowlist):
        lic_ok, lic_chosen, unallowed = self.license_node.evaluate(allowlist)
        exc_ok = (self.exception_ident == 'LLVM-exception')
        if lic_ok and exc_ok:
            return True, f'{lic_chosen} WITH {self.exception_ident}', []
        res_unallowed = list(unallowed)
        if not exc_ok:
            res_unallowed.append(f'WITH {self.exception_ident}')
        return False, None, res_unallowed


class AndNode:
    def __init__(self, left, right):
        self.left = left
        self.right = right

    def evaluate(self, allowlist):
        l_ok, l_chosen, l_un = self.left.evaluate(allowlist)
        r_ok, r_chosen, r_un = self.right.evaluate(allowlist)
        if l_ok and r_ok:
            return True, f'{l_chosen} AND {r_chosen}', []
        return False, None, l_un + r_un


class OrNode:
    def __init__(self, left, right):
        self.left = left
        self.right = right

    def evaluate(self, allowlist):
        l_ok, l_chosen, l_un = self.left.evaluate(allowlist)
        r_ok, r_chosen, r_un = self.right.evaluate(allowlist)
        if l_ok and r_ok:
            score_l = preference_score(l_chosen)
            score_r = preference_score(r_chosen)
            chosen = l_chosen if score_l <= score_r else r_chosen
            return True, chosen, []
        if l_ok:
            return True, l_chosen, []
        if r_ok:
            return True, r_chosen, []
        return False, None, l_un + r_un


class Parser:
    def __init__(self, tokens):
        self.tokens = tokens
        self.pos = 0

    def peek(self):
        if self.pos < len(self.tokens):
            return self.tokens[self.pos]
        return None

    def consume(self, expected_kind=None):
        tok = self.peek()
        if not tok:
            raise ValueError('Unexpected end of expression')
        if expected_kind and tok[0] != expected_kind:
            raise ValueError(f'Expected {expected_kind}, got {tok[0]} ({tok[1]})')
        self.pos += 1
        return tok

    def parse(self):
        res = self.parse_or()
        if self.pos < len(self.tokens):
            extra = self.tokens[self.pos]
            raise ValueError(f'Unexpected trailing token: {extra[1]}')
        return res

    def parse_or(self):
        node = self.parse_and()
        while True:
            tok = self.peek()
            if tok and tok[0] == 'OR':
                self.consume('OR')
                right = self.parse_and()
                node = OrNode(node, right)
            else:
                break
        return node

    def parse_and(self):
        node = self.parse_with()
        while True:
            tok = self.peek()
            if tok and tok[0] == 'AND':
                self.consume('AND')
                right = self.parse_with()
                node = AndNode(node, right)
            else:
                break
        return node

    def parse_with(self):
        node = self.parse_primary()
        tok = self.peek()
        if tok and tok[0] == 'WITH':
            self.consume('WITH')
            exc_tok = self.consume('IDENT')
            node = WithNode(node, exc_tok[1])
        return node

    def parse_primary(self):
        tok = self.peek()
        if not tok:
            raise ValueError('Expected expression, got end of input')
        if tok[0] == 'LPAREN':
            self.consume('LPAREN')
            node = self.parse_or()
            self.consume('RPAREN')
            return node
        elif tok[0] == 'IDENT':
            self.consume('IDENT')
            return LicenseNode(tok[1])
        else:
            raise ValueError(f'Unexpected token in expression: {tok[1]}')


def tokenize(expr):
    # Normalize legacy Rust slash syntax e.g. "Apache-2.0 / MIT" or "Apache-2.0/MIT"
    expr = re.sub(r'\s*/\s*', ' OR ', expr)
    token_spec = [
        ('LPAREN', r'\('),
        ('RPAREN', r'\)'),
        ('AND', r'\bAND\b'),
        ('OR', r'\bOR\b'),
        ('WITH', r'\bWITH\b'),
        ('IDENT', r'[A-Za-z0-9_.\-+:]+'),
        ('SKIP', r'\s+'),
    ]
    tok_regex = '|'.join('(?P<%s>%s)' % pair for pair in token_spec)
    tokens = []
    for mo in re.finditer(tok_regex, expr):
        kind = mo.lastgroup
        value = mo.group()
        if kind == 'SKIP':
            continue
        tokens.append((kind, value))
    return tokens


def parse_spdx(expr_str, allowlist=None):
    """Evaluates an SPDX expression string against allowlist.

    Returns: (acceptable: bool, chosen: str | None, unallowed: list[str])
    """
    if allowlist is None:
        allowlist = ALLOWLIST
    if expr_str is None:
        return False, None, ['Лицензия не указана']
    clean_str = str(expr_str).strip()
    if not clean_str or clean_str == '-':
        return False, None, ['Лицензия не указана']
    tokens = tokenize(clean_str)
    if not tokens:
        return False, None, ['Лицензия не указана']
    try:
        parser = Parser(tokens)
        ast = parser.parse()
        return ast.evaluate(allowlist)
    except Exception as e:
        return False, None, [f'Ошибка разбора: {e}']


def rust_allowlist_dependency_phrase(count: int) -> str:
    """Фраза «все N … allowlist» с согласованием числительного и глагола."""
    if count % 100 in (11, 12, 13, 14):
        noun, verb = 'зависимостей', 'удовлетворяют'
    elif count % 10 == 1:
        noun, verb = 'зависимость', 'удовлетворяет'
    elif count % 10 in (2, 3, 4):
        noun, verb = 'зависимости', 'удовлетворяют'
    else:
        noun, verb = 'зависимостей', 'удовлетворяют'
    return f'все {count} {noun} {verb} allowlist'
