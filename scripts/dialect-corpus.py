#!/usr/bin/env python3
"""Runs the dialect corpus on real servers and records which of them accept each statement.

Every case of `crates/syntax/tests/data/dialects.sql`, and the example of every row of the feature
table, runs on SQLite (Python's own), MySQL 8.0 and 8.4, MariaDB 11 and PostgreSQL 18 in Docker,
each against the same fixture tables. Each case starts from a fresh database. What a server did is recorded as
`ok`, `syntax` (MySQL and MariaDB 1064, PostgreSQL 42601, SQLite "syntax error", "unrecognized
token" or "incomplete input") or `error:<code>` for any other error, in `dialects-verified.txt`.
`cargo test` holds the parser and the feature table to that record: a statement the parser accepts
must not be a syntax error on the server, and one it rejects must not run. Another error fits
either verdict, since a server may read a statement in another way than meant (MySQL reads
`a FULL JOIN b` as a table `a` with the alias `full`) and then fail on what follows.
`dialects-known.txt` lists the differences that are deliberate, with the reason. The cases where
the parser contradicts a server are printed.

    python3 scripts/dialect-corpus.py           # start the containers, run, stop them
    python3 scripts/dialect-corpus.py --keep    # leave the containers running for the next run
"""

import argparse
import concurrent.futures
import datetime
import json
import os
from pathlib import Path
import re
import sqlite3
import subprocess
import tempfile
import time

ROOT = Path(__file__).resolve().parent.parent
CORPUS = ROOT / 'crates/syntax/tests/data/dialects.sql'
VERIFIED = ROOT / 'crates/syntax/tests/data/dialects-verified.txt'
KNOWN = ROOT / 'crates/syntax/tests/data/dialects-known.txt'
PASSWORD = 'corpus'

CONTAINERS = {
    'mysql-8.0': ('sqlls-corpus-mysql80', 'mysql:8.0', 'MYSQL_ROOT_PASSWORD'),
    'mysql-8.4': ('sqlls-corpus-mysql84', 'mysql:8.4', 'MYSQL_ROOT_PASSWORD'),
    'mariadb': ('sqlls-corpus-mariadb', 'mariadb:11', 'MARIADB_ROOT_PASSWORD'),
    'postgres': ('sqlls-corpus-postgres', 'postgres:18', 'POSTGRES_PASSWORD'),
}

MYSQL_FIXTURES = """
CREATE TABLE t (id INT PRIMARY KEY, a INT, b INT, c INT, d INT, j JSON, ts TIMESTAMP NULL, name VARCHAR(100), x INT);
CREATE TABLE u (id INT PRIMARY KEY, a INT, x INT);
CREATE TABLE s (id INT, a INT, del BOOLEAN, gone BOOLEAN);
CREATE TABLE a (x INT, y INT, name TEXT, date DATE, value INT);
CREATE TABLE b (x INT, y INT);
"""

POSTGRES_FIXTURES = """
CREATE TABLE t (id INT PRIMARY KEY, a INT, b INT, c INT, d INT, j JSONB, ts TIMESTAMP, name VARCHAR(100), x INT, arr INT[]);
CREATE TABLE u (id INT PRIMARY KEY, a INT, x INT);
CREATE TABLE s (id INT, a INT, del BOOLEAN, gone BOOLEAN);
CREATE TABLE a (x INT, y INT, name TEXT, date DATE, value INT);
CREATE TABLE b (x INT, y INT);
"""

SQLITE_FIXTURES = """
CREATE TABLE t (id INTEGER PRIMARY KEY, a INT, b INT, c INT, d INT, j TEXT, ts TEXT, name TEXT, x INT);
CREATE TABLE u (id INTEGER PRIMARY KEY, a INT, x INT);
CREATE TABLE s (id INT, a INT, del INT, gone INT);
CREATE TABLE a (x INT, y INT, name TEXT, date TEXT, value INT);
CREATE TABLE b (x INT, y INT);
"""


def cargo_json(*args):
    output = subprocess.check_output(
        ['cargo', 'run', '-q', '-p', 'sql-syntax', '--example', 'corpus', '--', *args], cwd=ROOT, text=True
    )
    return json.loads(output)


def docker(*args, stdin=None):
    return subprocess.run(['docker', *args], input=stdin, capture_output=True, text=True)


def running(name):
    result = docker('ps', '--filter', f'name=^{name}$', '--format', '{{.Names}}')
    return result.stdout.strip() == name


def start_containers():
    started = []
    for name, image, variable in CONTAINERS.values():
        if not running(name):
            docker('run', '-d', '--rm', '--name', name, '-e', f'{variable}={PASSWORD}', image)
            started.append(name)
    deadline = time.monotonic() + 180
    for server, (name, _, _) in CONTAINERS.items():
        while True:
            if server == 'postgres':
                ready = docker('exec', name, 'psql', '-U', 'postgres', '-c', 'SELECT 1').returncode == 0
            else:
                ready = mysql(server, 'SELECT 1;')[0] == 0
            if ready:
                break
            if time.monotonic() > deadline:
                raise TimeoutError(f'{name} did not start')
            time.sleep(2)
    return started


def mysql(server, script):
    name = CONTAINERS[server][0]
    client = 'mariadb' if server == 'mariadb' else 'mysql'
    result = docker(
        'exec', '-i', '-e', f'MYSQL_PWD={PASSWORD}', name, client, '-uroot', '-N', '--default-character-set=utf8mb4',
        stdin=script,
    )
    return result.returncode, result.stdout, result.stderr


def run_mysql(server, text):
    prelude = 'DROP DATABASE IF EXISTS corpus; DROP DATABASE IF EXISTS s; CREATE DATABASE corpus; USE corpus;\n'
    prelude += MYSQL_FIXTURES.strip() + '\n'
    # A delimiter of its own lets a statement hold semicolons, as a compound statement does.
    prelude += 'DELIMITER $$$\n'
    first_line = prelude.count('\n') + 1
    _, _, stderr = mysql(server, prelude + text.rstrip().rstrip(';') + '\n$$$\n')
    for line in stderr.splitlines():
        found = re.match(r'ERROR (\d+) \(\w+\) at line (\d+)', line)
        if found and int(found.group(2)) >= first_line:
            code = found.group(1)
            return ('syntax' if code == '1064' else f'error:{code}'), line
        if found:
            raise RuntimeError(f'{server}: the fixtures failed: {line}')
    return 'ok', ''


def run_postgres(text):
    name = CONTAINERS['postgres'][0]
    result = docker(
        'exec', name, 'psql', '-U', 'postgres', '-X', '-q', '-c', 'DROP DATABASE IF EXISTS corpus WITH (FORCE)',
        '-c', 'CREATE DATABASE corpus',
    )
    if result.returncode != 0:
        raise RuntimeError(f'postgres: the database could not be made: {result.stderr}')
    prelude = '\\set VERBOSITY verbose\nSET client_min_messages = warning;\n' + POSTGRES_FIXTURES.strip() + '\n'
    first_line = prelude.count('\n') + 1
    result = docker('exec', '-i', name, 'psql', '-U', 'postgres', '-d', 'corpus', '-X', '-q', '-f', '-', stdin=prelude + text + '\n')
    for line in result.stderr.splitlines():
        found = re.match(r'psql:<stdin>:(\d+): ERROR:\s+(\w+):', line)
        if found and int(found.group(1)) >= first_line:
            code = found.group(2)
            return ('syntax' if code == '42601' else f'error:{code}'), line
        if found:
            raise RuntimeError(f'postgres: the fixtures failed: {line}')
    return 'ok', ''


def run_sqlite(text):
    connection = sqlite3.connect(':memory:')
    connection.executescript(SQLITE_FIXTURES)
    try:
        connection.executescript(text)
    except sqlite3.Error as error:
        message = str(error)
        syntax = 'syntax error' in message or 'unrecognized token' in message or 'incomplete input' in message
        return ('syntax' if syntax else 'error:' + type(error).__name__), message
    finally:
        connection.close()
    return 'ok', ''


def versions():
    found = {'sqlite': sqlite3.sqlite_version}
    for server in ('mysql-8.0', 'mysql-8.4', 'mariadb'):
        _, stdout, _ = mysql(server, 'SELECT VERSION();')
        found[server] = stdout.strip().split('-')[0]
    result = docker('exec', CONTAINERS['postgres'][0], 'psql', '-U', 'postgres', '-At', '-c', 'SHOW server_version')
    found['postgres'] = result.stdout.strip().split(' ')[0]
    return found


DIALECTS = {'sqlite': 'sqlite', 'mysql-8.0': 'mysql', 'mysql-8.4': 'mysql', 'mariadb': 'mariadb', 'postgres': 'postgres'}


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--keep', action='store_true', help='leave the containers running')
    parser.add_argument('--log', type=Path, help='write every outcome with the message of the server to this file')
    args = parser.parse_args()

    cases = cargo_json('cases', str(CORPUS))
    # SQLite writes the files of ATTACH where the process stands.
    workspace = tempfile.TemporaryDirectory(prefix='dialect-corpus-')
    os.chdir(workspace.name)
    started = start_containers()
    try:
        server_versions = versions()
        results = {server: {} for server in DIALECTS}

        def run(server):
            for case in cases:
                if server == 'sqlite':
                    results[server][case['id']] = run_sqlite(case['text'])
                elif server == 'postgres':
                    results[server][case['id']] = run_postgres(case['text'])
                else:
                    results[server][case['id']] = run_mysql(server, case['text'])

        with concurrent.futures.ThreadPoolExecutor(max_workers=len(DIALECTS)) as pool:
            for future in [pool.submit(run, server) for server in DIALECTS]:
                future.result()
    finally:
        if not args.keep:
            for name in started:
                docker('stop', name)

    specs = [f'{server}={DIALECTS[server]}@{server_versions[server]}' for server in DIALECTS]
    today = datetime.date.today().isoformat()
    lines = [
        f'# Written by scripts/dialect-corpus.py on {today}: the servers that accept each case of the corpus.',
        *[f'# server {spec}' for spec in specs],
    ]
    for case in cases:
        outcomes = ' '.join(f"{server}={results[server][case['id']][0]}" for server in DIALECTS)
        lines.append(f"{case['id']}\t{outcomes}")
    VERIFIED.write_text('\n'.join(lines) + '\n')
    if args.log:
        log = {case['id']: {server: results[server][case['id']] for server in DIALECTS} for case in cases}
        args.log.write_text(json.dumps(log, indent=1))

    known = set()
    for line in KNOWN.read_text().splitlines():
        if line.strip() and not line.startswith('#'):
            case, server, _ = line.split('\t', 2)
            known.add((case, server))
    ours = cargo_json('verdicts', str(CORPUS), *specs)
    counts = {'agree': 0, 'other error': 0, 'known': 0, 'contradict': 0}
    for case in cases:
        for server in DIALECTS:
            outcome, message = results[server][case['id']]
            accepts = ours[case['id']][server]
            if outcome.startswith('error'):
                counts['other error'] += 1
            elif accepts == (outcome == 'ok'):
                counts['agree'] += 1
            elif (case['id'], server) in known:
                counts['known'] += 1
            else:
                counts['contradict'] += 1
                verdict = 'runs' if outcome == 'ok' else 'rejects'
                print(f"{case['id']} on {server}: the server {verdict} {case['text']!r} {message}")
    summary = ', '.join(f'{count} {name}' for name, count in counts.items())
    print(f'{len(cases)} cases on {len(DIALECTS)} servers ({", ".join(specs)}): {summary}')


if __name__ == '__main__':
    main()
