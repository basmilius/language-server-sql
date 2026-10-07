#!/usr/bin/env python3
"""Runs the inspection corpus on real servers and records which of them reject each statement.

Every case of `crates/analysis/tests/data/inspections.sql` runs on SQLite (Python's own), MySQL 8.0
and 8.4, MariaDB 11 and PostgreSQL 18 in Docker, each in a fresh database after the fixture of its
dialect. What a server did is recorded as `ok`, `syntax` or `error:<code>` in
`inspections-verified.txt`. `cargo test` holds the inspections to that record: one that reports an
error claims the server rejects the statement, and the inspection a case names must report an error
exactly where the server rejects it. `inspections-known.txt` lists the differences that are
deliberate, with the reason.

    python3 scripts/inspection-corpus.py           # start the containers, run, stop them
    python3 scripts/inspection-corpus.py --keep    # leave the containers running for the next run
"""

import argparse
import concurrent.futures
import datetime
import importlib.util
import json
from pathlib import Path
import re
import sqlite3
import subprocess

ROOT = Path(__file__).resolve().parent.parent
CORPUS = ROOT / 'crates/analysis/tests/data/inspections.sql'
VERIFIED = ROOT / 'crates/analysis/tests/data/inspections-verified.txt'

# The containers, the clients and the version probe are the dialect corpus's.
_spec = importlib.util.spec_from_file_location('dialect_corpus', ROOT / 'scripts/dialect-corpus.py')
servers = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(servers)

DIALECTS = {'sqlite': 'sqlite', 'mysql-8.0': 'mysql', 'mysql-8.4': 'mysql', 'mariadb': 'mariadb', 'postgres': 'postgres'}


def corpus():
    output = subprocess.check_output(
        ['cargo', 'run', '-q', '-p', 'sql-analysis', '--example', 'inspection_corpus', '--', 'cases', str(CORPUS)],
        cwd=ROOT,
        text=True,
    )
    return json.loads(output)


def run_mysql(server, fixture, text):
    prelude = 'DROP DATABASE IF EXISTS corpus; CREATE DATABASE corpus; USE corpus;\n' + fixture.strip() + '\n'
    first_line = prelude.count('\n') + 1
    _, _, stderr = servers.mysql(server, prelude + text + '\n')
    for line in stderr.splitlines():
        found = re.match(r'ERROR (\d+) \(\w+\) at line (\d+)', line)
        if found and int(found.group(2)) >= first_line:
            code = found.group(1)
            return ('syntax' if code == '1064' else f'error:{code}'), line
        if found:
            raise RuntimeError(f'{server}: the fixture failed: {line}')
    return 'ok', ''


def run_postgres(fixture, text):
    name = servers.CONTAINERS['postgres'][0]
    result = servers.docker(
        'exec', name, 'psql', '-U', 'postgres', '-X', '-q', '-c', 'DROP DATABASE IF EXISTS corpus WITH (FORCE)',
        '-c', 'CREATE DATABASE corpus',
    )
    if result.returncode != 0:
        raise RuntimeError(f'postgres: the database could not be made: {result.stderr}')
    prelude = '\\set VERBOSITY verbose\nSET client_min_messages = warning;\n' + fixture.strip() + '\n'
    first_line = prelude.count('\n') + 1
    result = servers.docker(
        'exec', '-i', name, 'psql', '-U', 'postgres', '-d', 'corpus', '-X', '-q', '-f', '-', stdin=prelude + text + '\n'
    )
    for line in result.stderr.splitlines():
        found = re.match(r'psql:<stdin>:(\d+): ERROR:\s+(\w+):', line)
        if found and int(found.group(1)) >= first_line:
            code = found.group(2)
            return ('syntax' if code == '42601' else f'error:{code}'), line
        if found:
            raise RuntimeError(f'postgres: the fixture failed: {line}')
    return 'ok', ''


def run_sqlite(fixture, text):
    connection = sqlite3.connect(':memory:')
    connection.executescript(fixture)
    try:
        connection.executescript(text)
    except sqlite3.Error as error:
        message = str(error)
        syntax = 'syntax error' in message or 'unrecognized token' in message or 'incomplete input' in message
        return ('syntax' if syntax else 'error:' + type(error).__name__), message
    finally:
        connection.close()
    return 'ok', ''


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--keep', action='store_true', help='leave the containers running')
    parser.add_argument('--log', type=Path, help='write every outcome with the message of the server to this file')
    args = parser.parse_args()

    found = corpus()
    fixtures = found['fixtures']
    cases = found['cases']
    started = servers.start_containers()
    try:
        server_versions = servers.versions()
        results = {server: {} for server in DIALECTS}

        def run(server):
            dialect = DIALECTS[server]
            fixture = fixtures['mysql' if dialect in ('mysql', 'mariadb') else dialect]
            for case in cases:
                if dialect not in case['dialects']:
                    continue
                if server == 'sqlite':
                    results[server][case['id']] = run_sqlite(fixture, case['text'])
                elif server == 'postgres':
                    results[server][case['id']] = run_postgres(fixture, case['text'])
                else:
                    results[server][case['id']] = run_mysql(server, fixture, case['text'])

        with concurrent.futures.ThreadPoolExecutor(max_workers=len(DIALECTS)) as pool:
            for future in [pool.submit(run, server) for server in DIALECTS]:
                future.result()
    finally:
        if not args.keep:
            for name in started:
                servers.docker('stop', name)

    specs = [f'{server}={DIALECTS[server]}@{server_versions[server]}' for server in DIALECTS]
    today = datetime.date.today().isoformat()
    lines = [
        f'# Written by scripts/inspection-corpus.py on {today}: what each server did with each case.',
        *[f'# server {spec}' for spec in specs],
    ]
    for case in cases:
        outcomes = ' '.join(
            f"{server}={results[server][case['id']][0]}" for server in DIALECTS if case['id'] in results[server]
        )
        lines.append(f"{case['id']}\t{outcomes}")
    VERIFIED.write_text('\n'.join(lines) + '\n')
    if args.log:
        log = {
            case['id']: {server: results[server][case['id']] for server in DIALECTS if case['id'] in results[server]}
            for case in cases
        }
        args.log.write_text(json.dumps(log, indent=1))
    rejected = sum(1 for server in DIALECTS for outcome in results[server].values() if outcome[0] != 'ok')
    total = sum(len(results[server]) for server in DIALECTS)
    print(f'{len(cases)} cases, {total} runs on {", ".join(specs)}: {rejected} rejected')


if __name__ == '__main__':
    main()
