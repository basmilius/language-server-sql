#!/usr/bin/env python3
"""Collects the reserved words of each dialect from real servers into `crates/syntax/src/reserved_words.rs`.

A word counts as reserved when it cannot name a column without quotes. MySQL says so itself in
`information_schema.KEYWORDS`, and PostgreSQL in `pg_get_keywords()` (the reserved words and those
that may only name a function or a type). For MariaDB and SQLite every candidate word is tried as
`CREATE TABLE r (<word> INT)`; the candidates are every keyword the servers and the parser know.
The containers are those of `dialect-corpus.py`, which this script starts and stops the same way.

    python3 scripts/reserved-words.py [--keep]
"""

import argparse
import datetime
import importlib.util
from pathlib import Path
import re
import sqlite3

ROOT = Path(__file__).resolve().parent.parent
OUTPUT = ROOT / 'crates/syntax/src/reserved_words.rs'
KINDS = ROOT / 'crates/syntax/src/kind.rs'

SPEC = importlib.util.spec_from_file_location('corpus', Path(__file__).with_name('dialect-corpus.py'))
CORPUS = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(CORPUS)


def query_mysql(server, sql):
    _, stdout, _ = CORPUS.mysql(server, sql)
    return [line.strip() for line in stdout.splitlines() if line.strip()]


def mysql_reserved(server):
    return set(query_mysql(server, 'SELECT WORD FROM information_schema.KEYWORDS WHERE RESERVED = 1;'))


def mariadb_reserved(candidates):
    words = sorted(candidates)
    script = 'DROP DATABASE IF EXISTS words; CREATE DATABASE words; USE words;\n'
    first_line = script.count('\n') + 1
    script += ''.join(f'CREATE TABLE r{index} ({word} INT);\n' for index, word in enumerate(words))
    name = CORPUS.CONTAINERS['mariadb'][0]
    result = CORPUS.docker(
        'exec', '-i', '-e', f'MYSQL_PWD={CORPUS.PASSWORD}', name, 'mariadb', '-uroot', '--force', stdin=script
    )
    reserved = set()
    for line in result.stderr.splitlines():
        found = re.match(r'ERROR 1064 \(\w+\) at line (\d+)', line)
        if found:
            reserved.add(words[int(found.group(1)) - first_line])
    return reserved


def postgres_reserved():
    name = CORPUS.CONTAINERS['postgres'][0]
    result = CORPUS.docker(
        'exec', name, 'psql', '-U', 'postgres', '-At', '-c',
        "SELECT upper(word) FROM pg_get_keywords() WHERE catcode IN ('R', 'T')",
    )
    return {line.strip() for line in result.stdout.splitlines() if line.strip()}


def sqlite_reserved(candidates):
    reserved = set()
    connection = sqlite3.connect(':memory:')
    for index, word in enumerate(sorted(candidates)):
        try:
            connection.execute(f'CREATE TABLE r{index} ({word} INT)')
        except sqlite3.OperationalError as error:
            if 'syntax error' in str(error):
                reserved.add(word)
    return reserved


def rust_list(name, words, doc):
    lines = [f'/// {doc}', f'pub(crate) static {name}: &[&str] = &[']
    row = []
    for word in sorted(words):
        row.append(f'"{word}"')
        if len(', '.join(row)) > 100:
            lines.append('    ' + ', '.join(row[:-1]) + ',')
            row = row[-1:]
    if row:
        lines.append('    ' + ', '.join(row) + ',')
    lines.append('];')
    return '\n'.join(lines)


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--keep', action='store_true', help='leave the containers running')
    args = parser.parse_args()

    started = CORPUS.start_containers()
    try:
        versions = CORPUS.versions()
        mysql_80 = mysql_reserved('mysql-8.0')
        mysql_84 = mysql_reserved('mysql-8.4')
        mariadb_words = set(query_mysql('mariadb', 'SELECT WORD FROM information_schema.KEYWORDS;'))
        own = set(re.findall(r'\("([A-Z_]+)", SyntaxKind::', KINDS.read_text()))
        postgres = postgres_reserved()
        candidates = {word.upper() for word in mysql_80 | mysql_84 | mariadb_words | postgres | own if word.isidentifier()}
        mariadb = mariadb_reserved(candidates)
    finally:
        if not args.keep:
            for name in started:
                CORPUS.docker('stop', name)
    sqlite = sqlite_reserved(candidates)

    today = datetime.date.today().isoformat()
    parts = [
        '//! The words each dialect reserves: those that cannot name a column without quotes. Written by',
        f'//! `scripts/reserved-words.py` on {today} from SQLite {versions["sqlite"]}, MySQL {versions["mysql-8.0"]}'
        f' and {versions["mysql-8.4"]},',
        f'//! MariaDB {versions["mariadb"]} and PostgreSQL {versions["postgres"]}; do not edit by hand.',
        '',
        rust_list('SQLITE', sqlite, 'Reserved in SQLite.'),
        '',
        rust_list('MYSQL', mysql_80 & mysql_84, 'Reserved in MySQL 8.0 and 8.4.'),
        '',
        rust_list('MYSQL_8_0_ONLY', mysql_80 - mysql_84, 'Reserved in MySQL 8.0 and no longer in 8.4.'),
        '',
        rust_list('MYSQL_8_4', mysql_84 - mysql_80, 'Reserved since MySQL 8.4.'),
        '',
        rust_list('MARIADB', mariadb, 'Reserved in MariaDB.'),
        '',
        rust_list('POSTGRES', postgres, 'Reserved in PostgreSQL, or allowed only as the name of a function or a type.'),
        '',
    ]
    OUTPUT.write_text('\n'.join(parts))
    counts = {
        'sqlite': len(sqlite), 'mysql': len(mysql_80 & mysql_84), 'mysql 8.0 only': len(mysql_80 - mysql_84),
        'mysql 8.4': len(mysql_84 - mysql_80), 'mariadb': len(mariadb), 'postgres': len(postgres),
    }
    print(f'{OUTPUT.relative_to(ROOT)}: {counts}')


if __name__ == '__main__':
    main()
