"""Refuse a sleep in the test code of core and the Claude Code bridge, printing each as file, line and line text, then their count, on standard error."""
import bisect
import os
import re
import subprocess
import sys
from pathlib import Path
from typing import Dict, Iterable, List, NamedTuple, Optional, Sequence, Set, Tuple

REPOSITORY_ROOT = Path(__file__).resolve().parent.parent
SCANNED_ROOTS = ('core', 'bridges/claude')
TEST_DIRECTORY_NAME = 'tests'
RUST_SUFFIX = '.rs'
MOD_FILE_NAME = 'mod.rs'
MOD_RS_FILE_NAMES = (MOD_FILE_NAME, 'lib.rs', 'main.rs')
SLEEP_FUNCTION_NAMES = ('sleep_until', 'sleep')
SHELL_SLEEP_PROGRAM = 'sleep'
LEXEME_PATTERN = re.compile(r'''
    (?P<line_comment>//[^\n]*)
  | (?P<block_comment>/\*)
  | (?P<raw_string>(?<![\w])[bc]?r(?P<hashes>\#*)"(?P<raw_body>.*?)"(?P=hashes))
  | (?P<string>(?:(?<![\w])[bc])?"(?P<body>(?:\\.|[^"\\])*)")
  | (?P<character>(?:(?<![\w])b)?'(?:\\(?:x[0-9a-fA-F]{2}|u\{[0-9a-fA-F]{1,6}\}|.)|[^\\'\n])')
''', re.VERBOSE | re.DOTALL)
BLOCK_COMMENT_DELIMITER_PATTERN = re.compile(r'/\*|\*/')
BLANKED_CHARACTER_PATTERN = re.compile(r'[^\n]')
BRACKET_PATTERN = re.compile(r'[(\[{]|[)\]}]')
OPENING_BRACKETS = '([{'
GROUP_OPENERS = '(['
GROUP_CLOSERS = ')]'
ITEM_TERMINATOR = ';'
BODY_OPENER = '{'
BODY_CLOSER = '}'
BLOCK_COMMENT_OPENER = '/*'
ITEM_DELIMITER_PATTERN = re.compile(r'[(\[{;]|[)\]}]')
ATTRIBUTE_START_PATTERN = re.compile(r'#\s*(?P<inner>!?)\s*\[')
TEST_FUNCTION_ATTRIBUTE_PATTERN = re.compile(r'\s*(?:\w+\s*::\s*)*test\s*(?:\(.*\))?\s*', re.DOTALL)
CONFIGURATION_ATTRIBUTE_PATTERN = re.compile(r'\s*cfg\s*\((?P<predicate>.*)\)\s*', re.DOTALL)
NEGATION_START_PATTERN = re.compile(r'\bnot\s*\(')
TEST_PREDICATE_PATTERN = re.compile(r'\btest\b')
PATH_ATTRIBUTE_PATTERN = re.compile(r'\s*path\s*=\s*"(?P<path>[^"]*)"\s*')
MODULE_DECLARATION_PATTERN = re.compile(r'\bmod\s+(?P<name>\w+)\s*;')
INLINE_MODULE_PATTERN = re.compile(r'\bmod\s+(?P<name>\w+)\s*\{')
USE_STATEMENT_PATTERN = re.compile(r'\buse\b[^;]*;')
SLEEP_ALIAS_PATTERN = re.compile(rf"\b(?:{'|'.join(SLEEP_FUNCTION_NAMES)})\s+as\s+(?P<alias>[A-Za-z]\w*)")
SHELL_SLEEP_COMMAND_PATTERN = re.compile(rf'''{SHELL_SLEEP_PROGRAM}\s+(?:[\d.${{"']|infinity\b)''')
SHELL_SLEEP_PROGRAM_PATTERN = re.compile(rf'(?:\S*/)?{SHELL_SLEEP_PROGRAM}')
SCRIPT_COMMENT_PATTERN = re.compile(r'(?:^|\s)#.*')
STRING_ESCAPE_PATTERN = re.compile(r'\\(?:\n\s*|x[0-9a-fA-F]{2}|u\{[0-9a-fA-F]{1,6}\}|.)', re.DOTALL)
LINE_CONTINUATION_START = '\\\n'
BYTE_ESCAPE_START = '\\x'
UNICODE_ESCAPE_START = '\\u{'
UNICODE_ESCAPE_END = '}'
HEXADECIMAL_BASE = 16
CHARACTER_BY_ESCAPE = {'n': '\n', 't': '\t', 'r': '\r', '0': '\0'}


class StringLiteral(NamedTuple):
    offset: int
    text: str
    is_raw: bool


class LexedRust(NamedTuple):
    code: str
    literals: Sequence[StringLiteral]


class Span(NamedTuple):
    start: int
    end: int


class AttributeRun(NamedTuple):
    span: Span
    marks_test: bool
    path: Optional[str]


class ModuleDeclaration(NamedTuple):
    name: str
    offset: int
    path: Optional[str]
    enclosing_inline_modules: Sequence[str]


class ParsedRust(NamedTuple):
    lexed: LexedRust
    test_spans: Sequence[Span]
    declarations: Sequence[ModuleDeclaration]


class Finding(NamedTuple):
    file: Path
    line: int


class UnresolvedTestModule(Exception):
    """A test module declaration names no file of the tree."""


def blank(text: str) -> str:
    """Replace every character but a newline with a space, so offsets and line numbers hold."""
    return BLANKED_CHARACTER_PATTERN.sub(' ', text)


def find_block_comment_end(text: str, start: int) -> int:
    """Return the offset just past the block comment opened at the start, nested comments included."""
    depth = 0
    for delimiter in BLOCK_COMMENT_DELIMITER_PATTERN.finditer(text, start):
        depth += 1 if delimiter.group() == BLOCK_COMMENT_OPENER else -1
        if depth == 0:
            return delimiter.end()
    return len(text)


def lex_rust(text: str) -> LexedRust:
    """Return the Rust text with every comment and literal blanked, beside the string literals it holds."""
    code_parts: List[str] = []
    literals: List[StringLiteral] = []
    position = 0
    while (lexeme := LEXEME_PATTERN.search(text, position)) is not None:
        end = find_block_comment_end(text, lexeme.start()) if lexeme.group('block_comment') else lexeme.end()
        is_raw = lexeme.group('raw_string') is not None
        body_group = 'raw_body' if is_raw else 'body' if lexeme.group('string') is not None else None
        if body_group is not None:
            literals.append(StringLiteral(lexeme.start(body_group), lexeme.group(body_group), is_raw))
        code_parts.append(text[position:lexeme.start()])
        code_parts.append(blank(text[lexeme.start():end]))
        position = end
    code_parts.append(text[position:])
    return LexedRust(''.join(code_parts), literals)


def find_closing_bracket(code: str, opening: int) -> int:
    """Return the offset just past the bracket that closes the one at the opening offset."""
    depth = 0
    for bracket in BRACKET_PATTERN.finditer(code, opening):
        depth += 1 if bracket.group() in OPENING_BRACKETS else -1
        if depth == 0:
            return bracket.end()
    return len(code)


def find_item_end(code: str, start: int) -> int:
    """Return the offset just past the item that starts at the offset: its first semicolon or braced body outside parentheses and brackets."""
    depth = 0
    for delimiter in ITEM_DELIMITER_PATTERN.finditer(code, start):
        token = delimiter.group()
        if token in GROUP_OPENERS:
            depth += 1
        elif token in GROUP_CLOSERS:
            depth -= 1
        elif depth == 0 and token == ITEM_TERMINATOR:
            return delimiter.end()
        elif depth == 0 and token == BODY_OPENER:
            return find_closing_bracket(code, delimiter.start())
        elif depth == 0 and token == BODY_CLOSER:
            return delimiter.start()
    return len(code)


def strip_negations(predicate: str) -> str:
    """Return a cfg predicate without any of its not(...) groups."""
    while (negation := NEGATION_START_PATTERN.search(predicate)) is not None:
        predicate = predicate[:negation.start()] + predicate[find_closing_bracket(predicate, negation.end() - 1):]
    return predicate


def marks_test(attribute: str) -> bool:
    """Whether an attribute's content compiles its item for tests: a test function attribute, or a cfg naming test outside a not."""
    if TEST_FUNCTION_ATTRIBUTE_PATTERN.fullmatch(attribute):
        return True
    configuration = CONFIGURATION_ATTRIBUTE_PATTERN.fullmatch(attribute)
    return configuration is not None and TEST_PREDICATE_PATTERN.search(strip_negations(configuration.group('predicate'))) is not None


def read_path_attribute(attribute_text: str) -> Optional[str]:
    """Return the file a path attribute names, or none for any other attribute."""
    path_attribute = PATH_ATTRIBUTE_PATTERN.fullmatch(attribute_text)
    return path_attribute.group('path') if path_attribute else None


def find_attribute_runs(text: str, code: str) -> Tuple[Sequence[AttributeRun], bool]:
    """Return each run of outer attributes with the item it applies to, and whether an inner attribute compiles the whole file for tests alone."""
    runs: List[AttributeRun] = []
    whole_file_is_test = False
    run_start: Optional[int] = None
    run_marks_test = False
    run_path: Optional[str] = None
    previous_end = -1
    for start in ATTRIBUTE_START_PATTERN.finditer(code):
        if start.start() < previous_end:
            continue
        end = find_closing_bracket(code, start.end() - 1)
        content = code[start.end():end - 1]
        if start.group('inner'):
            whole_file_is_test = whole_file_is_test or marks_test(content)
            previous_end = end
            continue
        if run_start is None or code[previous_end:start.start()].strip():
            if run_start is not None:
                runs.append(AttributeRun(Span(run_start, find_item_end(code, previous_end)), run_marks_test, run_path))
            run_start, run_marks_test, run_path = start.start(), False, None
        run_marks_test = run_marks_test or marks_test(content)
        run_path = read_path_attribute(text[start.end():end - 1]) or run_path
        previous_end = end
    if run_start is not None:
        runs.append(AttributeRun(Span(run_start, find_item_end(code, previous_end)), run_marks_test, run_path))
    return runs, whole_file_is_test


def find_inline_modules(code: str) -> Sequence[Tuple[str, Span]]:
    """Return the name and braced body of every inline module."""
    return [(module.group('name'), Span(module.end() - 1, find_closing_bracket(code, module.end() - 1))) for module in INLINE_MODULE_PATTERN.finditer(code)]


def find_module_declarations(code: str, runs: Sequence[AttributeRun]) -> Sequence[ModuleDeclaration]:
    """Return every module declaration that names a file, with its path attribute and the inline modules around it."""
    path_by_item_end = {run.span.end: run.path for run in runs}
    inline_modules = find_inline_modules(code)
    return [
        ModuleDeclaration(
            declaration.group('name'),
            declaration.start(),
            path_by_item_end.get(declaration.end()),
            [name for name, body in inline_modules if body.start < declaration.start() < body.end],
        )
        for declaration in MODULE_DECLARATION_PATTERN.finditer(code)
    ]


def parse_rust(text: str) -> ParsedRust:
    """Parse a Rust file into its lexed text, the spans compiled for tests alone and its module declarations."""
    lexed = lex_rust(text)
    runs, whole_file_is_test = find_attribute_runs(text, lexed.code)
    test_spans = [Span(0, len(text))] if whole_file_is_test else [run.span for run in runs if run.marks_test]
    return ParsedRust(lexed, test_spans, find_module_declarations(lexed.code, runs))


def is_in_spans(offset: int, spans: Sequence[Span]) -> bool:
    """Whether the offset falls inside any of the spans."""
    return any(span.start <= offset < span.end for span in spans)


def is_under_test_directory(file: Path) -> bool:
    """Whether the file sits under a tests directory."""
    return TEST_DIRECTORY_NAME in file.parent.parts


def module_directory(file: Path) -> Path:
    """Return the directory where the modules a file declares live."""
    return file.parent if file.name in MOD_RS_FILE_NAMES else file.parent / file.stem


def normalize(path: Path) -> Path:
    """Collapse the parent steps of a relative path."""
    return Path(os.path.normpath(path))


def resolve_module_file(file: Path, declaration: ModuleDeclaration, files: Set[Path]) -> Path:
    """Return the file a module declaration names, refusing one that names no file of the tree."""
    inline_directory = module_directory(file).joinpath(*declaration.enclosing_inline_modules)
    if declaration.path is not None:
        directory = inline_directory if declaration.enclosing_inline_modules else file.parent
        candidates = [normalize(directory / declaration.path)]
    else:
        candidates = [inline_directory / f'{declaration.name}{RUST_SUFFIX}', inline_directory / declaration.name / MOD_FILE_NAME]
    for candidate in candidates:
        if candidate in files:
            return candidate
    raise UnresolvedTestModule(f'{file}: the test module {declaration.name} names no file of the tree; looked for {", ".join(map(str, candidates))}')


def files_beneath(directory: Path, files: Iterable[Path]) -> Set[Path]:
    """Return every file under the directory."""
    return {file for file in files if directory in file.parents}


def collect_test_files(parsed: Dict[Path, ParsedRust], files: Set[Path]) -> Set[Path]:
    """Return every file compiled for tests alone: under a tests directory, or named by a test module declaration, with every file beneath it."""
    test_files = {file for file in files if is_under_test_directory(file)}
    while True:
        named = {
            resolve_module_file(file, declaration, files)
            for file, parsed_file in parsed.items()
            for declaration in parsed_file.declarations
            if (file in test_files or is_in_spans(declaration.offset, parsed_file.test_spans))
            and not (declaration.path is None and is_under_test_directory(file))
        }
        added = named - test_files
        if not added:
            return test_files
        test_files |= added
        for file in added:
            test_files |= files_beneath(module_directory(file), files)


def collect_sleep_aliases(use_statements: Iterable[str]) -> Set[str]:
    """Return every name the use statements give a sleep function."""
    return {alias.group('alias') for statement in use_statements for alias in SLEEP_ALIAS_PATTERN.finditer(statement)}


def compile_sleep_reference_pattern(aliases: Set[str]) -> re.Pattern:
    """Return the pattern of a sleep named in code, never as a method, a field or a definition: a sleep function bare or at the end of a path, or an alias bare."""
    function_names = '|'.join(SLEEP_FUNCTION_NAMES)
    alias_names = '|'.join(map(re.escape, sorted(aliases, key=len, reverse=True)))
    alias_alternative = rf"|(?<![\w.':])(?:{alias_names})" if aliases else ''
    return re.compile(rf"(?P<definition>\bfn\s+)?(?:(?<![\w.'])(?:{function_names}){alias_alternative})\b(?!\s*:(?!:))")


class LineIndex:
    """Turns an offset of a text into its line number."""

    def __init__(self, text: str):
        """Note where each line of the text starts."""
        self.line_starts = [0] + [match.end() for match in re.finditer('\n', text)]

    def line_of(self, offset: int) -> int:
        """Return the one-based line number of the offset."""
        return bisect.bisect_right(self.line_starts, offset)


def decode_escape(escape: str) -> str:
    """Return the characters one escape of a Rust string literal stands for, none for a line continuation."""
    if escape.startswith(LINE_CONTINUATION_START):
        return ''
    if escape.startswith(BYTE_ESCAPE_START):
        return chr(int(escape[len(BYTE_ESCAPE_START):], HEXADECIMAL_BASE))
    if escape.startswith(UNICODE_ESCAPE_START):
        return chr(int(escape[len(UNICODE_ESCAPE_START):-len(UNICODE_ESCAPE_END)], HEXADECIMAL_BASE))
    return CHARACTER_BY_ESCAPE.get(escape[1], escape[1])


def decode_string_literal(literal: StringLiteral) -> Tuple[str, Sequence[int]]:
    """Return the value a string literal holds, beside the offset in its source text that each character of the value comes from."""
    if literal.is_raw:
        return literal.text, range(len(literal.text))
    characters: List[str] = []
    source_offsets: List[int] = []
    position = 0
    for escape in STRING_ESCAPE_PATTERN.finditer(literal.text):
        characters.append(literal.text[position:escape.start()])
        source_offsets.extend(range(position, escape.start()))
        decoded = decode_escape(escape.group())
        characters.append(decoded)
        source_offsets.extend([escape.start()] * len(decoded))
        position = escape.end()
    characters.append(literal.text[position:])
    source_offsets.extend(range(position, len(literal.text)))
    return ''.join(characters), source_offsets


def find_shell_sleep(literal: StringLiteral) -> Optional[int]:
    """Return the source offset of the shell sleep a string literal's value holds, or none."""
    value, source_offsets = decode_string_literal(literal)
    command = SHELL_SLEEP_COMMAND_PATTERN.search(value)
    if command is not None:
        return literal.offset + source_offsets[command.start()]
    return literal.offset if SHELL_SLEEP_PROGRAM_PATTERN.fullmatch(value.strip()) else None


def find_rust_sleep_lines(parsed: ParsedRust, text: str) -> Set[int]:
    """Return the line of every sleep the test spans of a Rust file name in code or run through a shell string."""
    code = parsed.lexed.code
    lines = LineIndex(text)
    use_matches = list(USE_STATEMENT_PATTERN.finditer(code))
    use_statements = [Span(statement.start(), statement.end()) for statement in use_matches]
    sleep_reference = compile_sleep_reference_pattern(collect_sleep_aliases(statement.group() for statement in use_matches))
    code_offsets = [
        reference.start()
        for reference in sleep_reference.finditer(code)
        if reference.group('definition') is None and not is_in_spans(reference.start(), use_statements)
    ]
    literal_offsets = [offset for offset in map(find_shell_sleep, parsed.lexed.literals) if offset is not None]
    return {lines.line_of(offset) for offset in code_offsets + literal_offsets if is_in_spans(offset, parsed.test_spans)}


def find_script_sleep_lines(text: str) -> Set[int]:
    """Return the line of every shell sleep a script runs, its comments left out."""
    return {number for number, line in enumerate(text.split('\n'), start=1) if SHELL_SLEEP_COMMAND_PATTERN.search(SCRIPT_COMMENT_PATTERN.sub('', line))}


def find_sleeps(texts: Dict[Path, str]) -> Sequence[Finding]:
    """Return every sleep in the test code of the texts, ordered by file and line."""
    files = set(texts)
    parsed = {file: parse_rust(text) for file, text in texts.items() if file.suffix == RUST_SUFFIX}
    test_files = collect_test_files(parsed, files)
    findings = []
    for file, text in texts.items():
        if file in parsed and file in test_files:
            lines = find_rust_sleep_lines(parsed[file]._replace(test_spans=[Span(0, len(text))]), text)
        elif file in parsed:
            lines = find_rust_sleep_lines(parsed[file], text)
        elif file in test_files:
            lines = find_script_sleep_lines(text)
        else:
            lines = set()
        findings.extend(Finding(file, line) for line in lines)
    return sorted(findings)


def list_scanned_files(root: Path) -> Sequence[Path]:
    """Return every file of the scanned roots the tree holds, tracked or not ignored, relative to the root."""
    listing = subprocess.run(
        ['git', '-C', str(root), 'ls-files', '-z', '--cached', '--others', '--exclude-standard', '--', *SCANNED_ROOTS],
        check=True, capture_output=True, text=True,
    )
    return [Path(name) for name in listing.stdout.split('\0') if name and (root / name).is_file()]


def render_finding(finding: Finding, texts: Dict[Path, str]) -> str:
    """Render one sleep as its file, its line number and the line's text."""
    line_text = texts[finding.file].split('\n')[finding.line - 1].strip()
    return f'{finding.file}:{finding.line}: {line_text}'


def main() -> int:
    """Read the scanned roots, print every sleep in their test code and its count on standard error, and exit nonzero when there is one."""
    texts = {file: (REPOSITORY_ROOT / file).read_text(encoding='utf-8', errors='replace') for file in list_scanned_files(REPOSITORY_ROOT)}
    try:
        findings = find_sleeps(texts)
    except UnresolvedTestModule as unresolved:
        sys.stderr.write(f'{unresolved}\n')
        return 2
    for finding in findings:
        sys.stderr.write(render_finding(finding, texts) + '\n')
    if findings:
        sys.stderr.write(f'sleeps in test code: {len(findings)}\n')
    return 1 if findings else 0


if __name__ == '__main__':
    sys.exit(main())
