"""Run the two commands of the heavy gate under a machine-wide lock and write one evidence file for the run."""
import argparse
import fcntl
import os
import re
import subprocess
import sys
import time
from contextlib import contextmanager
from datetime import datetime, timezone
from pathlib import Path
from typing import IO, Callable, Iterator, NamedTuple, Sequence

REPOSITORY_ROOT = Path(__file__).resolve().parent.parent
DEFAULT_EVIDENCE_DIRECTORY = REPOSITORY_ROOT / 'target' / 'heavy-evidence'
HEAVY_GATE_LOCK_PATH = Path.home() / '.cache' / 'estafette' / 'heavy-gate.lock'
LOCK_POLL_SECONDS = 1
LOCK_WAIT_REPORT_SECONDS = 60
UNKNOWN_HOLDER = 'unknown (its record is not written yet)'
EVIDENCE_SUFFIX = '.txt'
LOG_SUFFIX = '.log'
PARTIAL_SUFFIX = '.partial'
FILE_NAME_TIME_FORMAT = '%Y%m%dT%H%M%SZ'
SHORT_COMMIT_LENGTH = 12
TEST_COMMAND = ('just', 'gate::test')
AUDIT_COMMAND = ('just', 'gate::audit')
CLEAN_WORKTREE_WORD = 'clean'
WORKTREE_STATUS_LABEL = 'worktree status:'
NO_ENTRIES_WORD = 'none'
TEST_BINARY_LINE_PATTERN = re.compile(r'^\s*(Running|Doc-tests)\s')
TEST_RESULT_LINE_PATTERN = re.compile(r'^test result:')
IGNORED_TEST_LINE_PATTERN = re.compile(r'^test (.+?) \.\.\. ignored')


class HeadIdentity(NamedTuple):
    commit: str
    tree: str
    status: str


class CommandRun(NamedTuple):
    command: Sequence[str]
    exit_code: int
    lines: Sequence[str]


class TestResult(NamedTuple):
    binary_line: str
    result_line: str


def run_git(arguments: Sequence[str]) -> str:
    """Return the standard output of one git command run at the repository root, without its final newline."""
    completed = subprocess.run(['git', '-C', str(REPOSITORY_ROOT), *arguments], check=True, capture_output=True, text=True)
    return completed.stdout.rstrip('\n')


def read_head_identity() -> HeadIdentity:
    """Read the commit, the tree and the short status of the checkout that is about to be tested."""
    return HeadIdentity(run_git(['rev-parse', 'HEAD']), run_git(['rev-parse', 'HEAD^{tree}']), run_git(['status', '--short']))


def run_command_into_log(command: Sequence[str], log: IO[str]) -> CommandRun:
    """Run a command, echo its output to standard error, append it to the log and keep its lines."""
    log.write('== ' + ' '.join(command) + '\n')
    lines = []
    with subprocess.Popen(command, cwd=REPOSITORY_ROOT, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, errors='replace') as process:
        for line in process.stdout:
            sys.stderr.write(line)
            sys.stderr.flush()
            log.write(line)
            log.flush()
            lines.append(line.rstrip('\n'))
    return CommandRun(command, process.returncode, lines)


def find_test_results(lines: Sequence[str]) -> Sequence[TestResult]:
    """Pair each test result line with the line that names the test binary it came from."""
    results = []
    binary_line = ''
    for line in lines:
        if TEST_BINARY_LINE_PATTERN.match(line):
            binary_line = line.strip()
        elif TEST_RESULT_LINE_PATTERN.match(line):
            results.append(TestResult(binary_line, line))
    return results


def find_ignored_tests(lines: Sequence[str]) -> Sequence[str]:
    """Return the name of every test the run printed as ignored."""
    matches = (IGNORED_TEST_LINE_PATTERN.match(line) for line in lines)
    return [match.group(1) for match in matches if match]


def render_worktree_status(status: str) -> str:
    """Say clean for an empty short status and list the paths below the label otherwise."""
    return f'{WORKTREE_STATUS_LABEL} {CLEAN_WORKTREE_WORD}' if not status else f'{WORKTREE_STATUS_LABEL}\n{status}'


def render_entries(entries: Sequence[str]) -> str:
    """Render one entry per line, or the word none for no entries."""
    return '\n'.join(entries) if entries else NO_ENTRIES_WORD


def render_holder_record() -> str:
    """Name this process, its checkout and the moment it took the lock."""
    since = datetime.now().astimezone().isoformat(timespec='seconds')
    return f'pid {os.getpid()}, checkout {REPOSITORY_ROOT}, since {since}'


def read_holder_record(lock_path: Path) -> str:
    """Return the record the lock's holder wrote, or say it is unknown."""
    return lock_path.read_text().strip() or UNKNOWN_HOLDER


@contextmanager
def hold_exclusive_lock(lock_path: Path, render_record: Callable[[], str]) -> Iterator[int]:
    """Hold an exclusive lock the operating system releases with the process, yielding the seconds waited for it."""
    lock_path.parent.mkdir(parents=True, exist_ok=True)
    with lock_path.open('a+') as lock_file:
        waiting_since = time.monotonic()
        report_after = 0.0
        while True:
            try:
                fcntl.flock(lock_file, fcntl.LOCK_EX | fcntl.LOCK_NB)
                break
            except BlockingIOError:
                waited = time.monotonic() - waiting_since
                if waited >= report_after:
                    sys.stderr.write(f'waiting for the heavy gate lock held by {read_holder_record(lock_path)}; waited {round(waited)} s\n')
                    report_after = waited + LOCK_WAIT_REPORT_SECONDS
                time.sleep(LOCK_POLL_SECONDS)
        seconds_waited = round(time.monotonic() - waiting_since)
        lock_file.seek(0)
        lock_file.truncate()
        lock_file.write(render_record())
        lock_file.flush()
        yield seconds_waited


def render_evidence(identity: HeadIdentity, seconds_waited: int, started: datetime, ended: datetime, test_run: CommandRun, audit_run: CommandRun, log_path: Path) -> str:
    """Render the evidence file of one heavy run."""
    results = find_test_results(test_run.lines)
    result_entries = [f'{result.binary_line}\n    {result.result_line}' for result in results]
    return '\n'.join([
        f'commit: {identity.commit}',
        f'tree: {identity.tree}',
        render_worktree_status(identity.status),
        f'seconds waited for the lock: {seconds_waited}',
        f'started: {started.isoformat(timespec="seconds")}',
        f'ended: {ended.isoformat(timespec="seconds")}',
        f'exit code of {" ".join(test_run.command)}: {test_run.exit_code}',
        f'exit code of {" ".join(audit_run.command)}: {audit_run.exit_code}',
        f'log: {log_path}',
        'test results:',
        render_entries(result_entries),
        'ignored tests:',
        render_entries(find_ignored_tests(test_run.lines)),
        '',
    ])


def choose_evidence_path(requested: str, started: datetime, commit: str) -> Path:
    """Return the requested path, or a path under target named for the start time and the commit."""
    if requested:
        return Path(requested).resolve()
    stamp = started.astimezone(timezone.utc).strftime(FILE_NAME_TIME_FORMAT)
    return DEFAULT_EVIDENCE_DIRECTORY / f'{stamp}-{commit[:SHORT_COMMIT_LENGTH]}{EVIDENCE_SUFFIX}'


def write_by_rename(path: Path, content: str) -> None:
    """Write the content beside the path and rename it over the path."""
    partial = path.with_name(path.name + PARTIAL_SUFFIX)
    partial.write_text(content)
    partial.replace(path)


def parse_arguments() -> argparse.Namespace:
    """Read the one optional argument: the path of the evidence file."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('evidence_path', nargs='?', default='')
    return parser.parse_args()


def main() -> int:
    """Wait for the lock, run the test command, then the audit command, write the evidence and print its path; exit with the first failing code."""
    arguments = parse_arguments()
    with hold_exclusive_lock(HEAVY_GATE_LOCK_PATH, render_holder_record) as seconds_waited:
        identity = read_head_identity()
        started = datetime.now().astimezone()
        evidence_path = choose_evidence_path(arguments.evidence_path, started, identity.commit)
        log_path = evidence_path.with_name(evidence_path.name + LOG_SUFFIX)
        evidence_path.parent.mkdir(parents=True, exist_ok=True)
        with log_path.open('w') as log:
            test_run = run_command_into_log(TEST_COMMAND, log)
            audit_run = run_command_into_log(AUDIT_COMMAND, log)
        ended = datetime.now().astimezone()
        write_by_rename(evidence_path, render_evidence(identity, seconds_waited, started, ended, test_run, audit_run, log_path))
    print(evidence_path)
    return test_run.exit_code or audit_run.exit_code


if __name__ == '__main__':
    sys.exit(main())
