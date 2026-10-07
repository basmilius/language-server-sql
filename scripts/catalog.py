#!/usr/bin/env python3
"""Takes the built-in catalogs of each dialect from the real servers.

What the servers have is taken from the servers themselves, version by version, and written to
`crates/catalog/data/<dialect>.tsv`, which `sql-catalog` embeds:

- functions: PostgreSQL's `pg_proc` (user-facing functions only), and for MySQL and MariaDB every
  candidate name prepared as a call on the server, which says whether it knows the function and
  which numbers of arguments it takes; SQLite's `pragma_function_list`;
- types: PostgreSQL's `pg_type`, and for MySQL and MariaDB every candidate type prepared in a
  `CREATE TABLE`;
- system relations and their columns: `information_schema`, `pg_catalog`, `mysql`,
  `performance_schema`, `sys`, and SQLite's schema tables and table-valued pragmas;
- settings: `pg_settings`, the system variables of MySQL and MariaDB, SQLite's pragmas.

The descriptions, and the signatures and return types the servers do not report, are written by
hand in `crates/catalog/data/descriptions.tsv`; this script only reads its names as candidates.

Each line of a data file is tab-separated:

    F  name  kind  params  returns  versions     a function overload (kind f, a, w, p)
    T  name  category  versions                  a data type
    R  schema  relation  kind  versions          a system table or view
    C  schema  relation  column  type  versions  a column of one
    V  name  type  versions                      a setting or variable

`params` is a comma-separated list of `name:type`, `...` before a variadic one and `?` after one
that may be left out. `versions` is `*`, `since-`, `-until` or `since-until` in major.minor of the
versions sampled.

    python3 scripts/catalog.py           # start the containers, take the catalogs, stop them
    python3 scripts/catalog.py --keep    # leave the containers running for the next run
"""

import argparse
import json
from pathlib import Path
import re
import subprocess
import time

ROOT = Path(__file__).resolve().parent.parent
DATA = ROOT / 'crates/catalog/data'
DESCRIPTIONS = DATA / 'descriptions.tsv'
PASSWORD = 'catalog'

SERVERS = {
    'postgres': [('18', 'sqlls-catalog-pg', 'postgres:18')],
    'mysql': [('8.0', 'sqlls-catalog-my80', 'mysql:8.0'), ('8.4', 'sqlls-catalog-my84', 'mysql:8.4')],
    'mariadb': [
        ('11.0', 'sqlls-catalog-ma110', 'mariadb:11.0'),
        ('11.4', 'sqlls-catalog-ma114', 'mariadb:11.4'),
        ('11.8', 'sqlls-catalog-ma11', 'mariadb:11'),
    ],
}

# Alpine releases and the SQLite they ship; the CLI's build has the math and JSON functions.
SQLITE = [('3.48', 'alpine:3.21'), ('3.49', 'alpine:3.22'), ('3.53', 'alpine:3.23')]

# Functions MySQL and MariaDB read in their grammar, which neither the help tables nor
# `SQL_FUNCTIONS` name.
GRAMMAR_FUNCTIONS = """
ascii bin binary_to_uuid bit_length char char_length character_length coalesce collation compress
concat concat_ws connection_id convert curdate current_date current_role current_time current_timestamp
current_user curtime database date date_add date_format date_sub datediff day dayname dayofmonth
dayofweek dayofyear default elt export_set extract field find_in_set format from_days from_unixtime
geomcollection geometrycollection get_format greatest grouping hex hour icu_version if ifnull insert
instr interval is_uuid json_table last_insert_id lcase least left length linestring localtime
localtimestamp locate lower lpad ltrim make_set match mid minute mod month monthname multilinestring
multipoint multipolygon now nullif oct octet_length ord point polygon position quarter quote regexp_like
repeat replace reverse right roles_graphml row_count rpad rtrim schema second sec_to_time session_user
sign space statement_digest statement_digest_text strcmp str_to_date subdate substr substring
substring_index subtime sysdate system_user time time_format time_to_sec timediff timestamp timestampadd
timestampdiff to_days to_seconds trim truncate ucase unhex unix_timestamp upper user utc_date utc_time
utc_timestamp uuid uuid_short uuid_to_bin validate_password_strength values version wait_for_executed_gtid_set
week weekday weekofyear weight_string year yearweek any_value bit_and bit_or bit_xor count group_concat
json_arrayagg json_objectagg max min std stddev stddev_pop stddev_samp sum var_pop var_samp variance avg
cume_dist dense_rank first_value lag last_value lead nth_value ntile percent_rank rank row_number
""".split()

MYSQL_TYPES = {
    'tinyint': 'numeric', 'smallint': 'numeric', 'mediumint': 'numeric', 'int': 'numeric',
    'integer': 'numeric', 'bigint': 'numeric', 'decimal(10,2)': 'numeric', 'dec': 'numeric',
    'numeric': 'numeric', 'fixed': 'numeric', 'float': 'numeric', 'double': 'numeric',
    'double precision': 'numeric', 'real': 'numeric', 'bit': 'numeric', 'bool': 'boolean',
    'boolean': 'boolean', 'serial': 'numeric', 'date': 'datetime', 'datetime': 'datetime',
    'timestamp': 'datetime', 'time': 'datetime', 'year': 'datetime', 'char': 'string',
    'varchar(255)': 'string', 'nchar': 'string', 'nvarchar(255)': 'string', 'national char': 'string',
    'national varchar(255)': 'string', 'binary': 'binary', 'varbinary(255)': 'binary',
    'tinyblob': 'binary', 'blob': 'binary', 'mediumblob': 'binary', 'longblob': 'binary',
    'tinytext': 'string', 'text': 'string', 'mediumtext': 'string', 'longtext': 'string',
    'long': 'string', 'long varchar': 'string', 'long varbinary': 'binary', "enum('a')": 'string',
    "set('a')": 'string', 'json': 'json', 'geometry': 'spatial', 'point': 'spatial',
    'linestring': 'spatial', 'polygon': 'spatial', 'multipoint': 'spatial',
    'multilinestring': 'spatial', 'multipolygon': 'spatial', 'geometrycollection': 'spatial',
    'geomcollection': 'spatial', 'inet4': 'network', 'inet6': 'network', 'uuid': 'string',
    'vector(3)': 'vector', 'xmltype': 'string',
}

SQLITE_TYPES = {
    'INTEGER': 'numeric', 'INT': 'numeric', 'TINYINT': 'numeric', 'SMALLINT': 'numeric',
    'MEDIUMINT': 'numeric', 'BIGINT': 'numeric', 'UNSIGNED BIG INT': 'numeric', 'INT2': 'numeric',
    'INT8': 'numeric', 'REAL': 'numeric', 'DOUBLE': 'numeric', 'DOUBLE PRECISION': 'numeric',
    'FLOAT': 'numeric', 'NUMERIC': 'numeric', 'DECIMAL': 'numeric', 'BOOLEAN': 'numeric',
    'DATE': 'numeric', 'DATETIME': 'numeric', 'TEXT': 'string', 'CHARACTER': 'string',
    'VARCHAR': 'string', 'VARYING CHARACTER': 'string', 'NCHAR': 'string', 'NATIVE CHARACTER': 'string',
    'NVARCHAR': 'string', 'CLOB': 'string', 'BLOB': 'binary', 'ANY': 'other',
}

# Spellings PostgreSQL accepts for its types, which `pg_type` holds under another name.
POSTGRES_TYPE_SPELLINGS = {
    'smallint': 'numeric', 'integer': 'numeric', 'bigint': 'numeric', 'decimal': 'numeric',
    'real': 'numeric', 'double precision': 'numeric', 'smallserial': 'numeric', 'serial': 'numeric',
    'bigserial': 'numeric', 'boolean': 'boolean', 'character': 'string', 'character varying': 'string',
    'timestamp with time zone': 'datetime', 'timestamp without time zone': 'datetime',
    'time with time zone': 'datetime', 'time without time zone': 'datetime', 'bit varying': 'bitstring',
}

POSTGRES_CATEGORIES = {
    'N': 'numeric', 'S': 'string', 'D': 'datetime', 'T': 'datetime', 'B': 'boolean', 'G': 'geometric',
    'I': 'network', 'R': 'range', 'V': 'bitstring', 'U': 'other', 'P': 'pseudo', 'A': 'array', 'E': 'enum',
    'C': 'composite', 'X': 'other',
}

POSTGRES_HIDDEN_TYPES = {
    'aclitem', 'cid', 'cstring', 'gtsvector', 'int2vector', 'oidvector', 'pg_brin_bloom_summary',
    'pg_brin_minmax_multi_summary', 'pg_dependencies', 'pg_mcv_list', 'pg_ndistinct', 'pg_node_tree',
    'internal', 'language_handler', 'fdw_handler', 'index_am_handler', 'tsm_handler', 'table_am_handler',
    'event_trigger', 'pg_ddl_command', 'unknown', 'tid', 'xid', 'xid8', 'regdictionary',
}

# Functions of `pg_catalog` that only the server's own machinery calls.
POSTGRES_HIDDEN_FUNCTIONS = re.compile(
    r'^(binary_upgrade_|pg_stat_get_|fmgr_|pg_isolation_test|pg_nextoid$|pg_stop_making_pinned_objects$'
    r'|currtid2$|amvalidate$|nameconcatoid$|int4inc$|int8_sum$|numeric_(div_trunc|exp|inc|ln|log|sqrt)$'
    r'|d(exp|log1|log10|round|trunc)$|textlen$|like_escape$|notlike$|like$|similar_escape$|shell_(in|out)$'
    r'|enum_(in|out|send)$|.*_(canonical|subdiff|validator)$|pclose$|popen$)'
)


def docker(*args, stdin=None):
    return subprocess.run(['docker', *args], input=stdin, capture_output=True, text=True, errors='replace')


def running(name):
    result = docker('ps', '--filter', f'name=^{name}$', '--format', '{{.Names}}')
    return result.stdout.strip() == name


def psql(name, query):
    result = docker('exec', '-i', name, 'psql', '-U', 'postgres', '-At', '-F', '\t', '-v', 'ON_ERROR_STOP=1', stdin=query)
    if result.returncode != 0:
        raise RuntimeError(result.stderr)
    return [line.split('\t') for line in result.stdout.splitlines() if line]


def mysql(dialect, name, script):
    client = 'mariadb' if dialect == 'mariadb' else 'mysql'
    result = docker('exec', '-i', '-e', f'MYSQL_PWD={PASSWORD}', name, client, '-uroot', '-N', '-B', '--default-character-set=utf8mb4', stdin=script)
    if result.returncode != 0:
        raise RuntimeError(result.stderr)
    return [line.split('\t') for line in result.stdout.splitlines() if line]


def start_containers():
    started = []
    for dialect, servers in SERVERS.items():
        variable = 'POSTGRES_PASSWORD' if dialect == 'postgres' else 'MYSQL_ROOT_PASSWORD'
        for _, name, image in servers:
            if not running(name):
                docker('run', '-d', '--rm', '--name', name, '-e', f'{variable}={PASSWORD}', image)
                started.append(name)
    deadline = time.monotonic() + 240
    for dialect, servers in SERVERS.items():
        for _, name, _ in servers:
            while True:
                try:
                    if dialect == 'postgres':
                        psql(name, 'SELECT 1;')
                    else:
                        mysql(dialect, name, 'SELECT 1;')
                    break
                except RuntimeError:
                    if time.monotonic() > deadline:
                        raise TimeoutError(f'{name} did not start')
                    time.sleep(2)
    return started


def described_names():
    names = set()
    if DESCRIPTIONS.exists():
        for line in DESCRIPTIONS.read_text().splitlines():
            if line and not line.startswith('#'):
                names.add(line.split('\t')[0].lower())
    return names


# PostgreSQL


def postgres_catalog(name):
    functions = []
    rows = psql(name, r"""
SELECT p.proname, p.prokind, p.proretset, pg_get_function_result(p.oid),
       array_to_json(coalesce(p.proargnames, '{}')), array_to_json(coalesce(p.proargmodes, '{}')),
       array_to_json(ARRAY(SELECT format_type(t, NULL) FROM unnest(p.proargtypes) WITH ORDINALITY AS u(t, n) ORDER BY n)),
       p.pronargdefaults, p.provariadic <> 0
FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace
WHERE n.nspname = 'pg_catalog'
  AND NOT (EXISTS (SELECT 1 FROM pg_operator o WHERE o.oprcode = p.oid)
      AND coalesce(obj_description(p.oid, 'pg_proc'), '') LIKE 'implementation of %')
  AND NOT EXISTS (SELECT 1 FROM pg_aggregate a WHERE p.oid IN (a.aggtransfn, a.aggfinalfn, a.aggcombinefn,
      a.aggserialfn, a.aggdeserialfn, a.aggmtransfn, a.aggminvtransfn, a.aggmfinalfn))
  AND NOT EXISTS (SELECT 1 FROM pg_type t WHERE p.oid IN (t.typinput, t.typoutput, t.typreceive, t.typsend,
      t.typmodin, t.typmodout, t.typanalyze, t.typsubscript))
  AND NOT EXISTS (SELECT 1 FROM pg_amproc ap WHERE ap.amproc = p.oid)
  AND NOT EXISTS (SELECT 1 FROM pg_cast c WHERE c.castfunc = p.oid)
  AND p.prorettype NOT IN ('internal'::regtype, 'trigger'::regtype, 'event_trigger'::regtype,
      'language_handler'::regtype, 'fdw_handler'::regtype, 'index_am_handler'::regtype,
      'table_am_handler'::regtype, 'tsm_handler'::regtype)
  AND NOT ('internal'::regtype = ANY (p.proargtypes))
  AND p.proname NOT LIKE '\_%'
ORDER BY 1;
""")
    for proname, prokind, retset, result, names, modes, types, ndefaults, variadic in rows:
        if POSTGRES_HIDDEN_FUNCTIONS.match(proname):
            continue
        names, modes, types = json.loads(names), json.loads(modes), json.loads(types)
        if modes:
            names = [name for name, mode in zip(names, modes) if mode in ('i', 'b', 'v')] if names else []
        params = []
        for index, type_name in enumerate(types):
            param_name = names[index] if index < len(names) else ''
            entry = f'{param_name}:{type_name}'
            if variadic == 't' and index == len(types) - 1:
                entry = '...' + entry
            if index >= len(types) - int(ndefaults):
                entry += '?'
            params.append(entry)
        functions.append((proname, prokind, ','.join(params), result))
    types = []
    for typname, category, typtype in psql(name, r"""
SELECT t.typname, t.typcategory, t.typtype FROM pg_type t JOIN pg_namespace n ON n.oid = t.typnamespace
WHERE n.nspname = 'pg_catalog' AND t.typtype IN ('b', 'r', 'm', 'p', 'd') AND t.typname NOT LIKE '\_%' AND t.typisdefined
ORDER BY 1;
"""):
        if typname in POSTGRES_HIDDEN_TYPES:
            continue
        types.append((typname, POSTGRES_CATEGORIES.get(category, 'other')))
    types.extend(POSTGRES_TYPE_SPELLINGS.items())
    relations, columns = system_relations_postgres(name)
    settings = [(setting, vartype) for setting, vartype in psql(name, 'SELECT name, vartype FROM pg_settings ORDER BY 1;')]
    return functions, types, relations, columns, settings


def system_relations_postgres(name):
    relations = [
        (schema, relation, 'view' if kind == 'VIEW' else 'table')
        for schema, relation, kind in psql(name, """
SELECT table_schema, table_name, table_type FROM information_schema.tables
WHERE table_schema IN ('pg_catalog', 'information_schema') ORDER BY 1, 2;
""")
    ]
    columns = psql(name, """
SELECT table_schema, table_name, column_name, CASE WHEN data_type = 'USER-DEFINED' OR data_type = 'ARRAY' THEN udt_name ELSE data_type END
FROM information_schema.columns WHERE table_schema IN ('pg_catalog', 'information_schema')
ORDER BY 1, 2, ordinal_position;
""")
    return relations, [tuple(column) for column in columns]


# MySQL and MariaDB

PROBE_PRELUDE = """
DROP DATABASE IF EXISTS catalog; CREATE DATABASE catalog; USE catalog;
CREATE TABLE probe_result (id INT PRIMARY KEY, code INT);
DELIMITER //
CREATE PROCEDURE probe(IN probe_id INT, IN probe_sql TEXT)
BEGIN
  DECLARE failed INT DEFAULT 0;
  DECLARE CONTINUE HANDLER FOR SQLEXCEPTION
  BEGIN
    GET DIAGNOSTICS CONDITION 1 @probe_code = MYSQL_ERRNO;
    SET failed = @probe_code;
  END;
  SET @probe_sql = probe_sql;
  PREPARE probe_stmt FROM @probe_sql;
  INSERT INTO probe_result VALUES (probe_id, failed);
END//
DELIMITER ;
"""

UNKNOWN_FUNCTION = {1305, 1630}
WRONG_COUNT = 1582
SYNTAX = 1064


def probe(dialect, name, statements):
    """Prepares each statement on the server and gives the error code of each, 0 when it prepared."""
    script = PROBE_PRELUDE
    for index, statement in enumerate(statements):
        quoted = statement.replace('\\', '\\\\').replace("'", "''")
        script += f"CALL probe({index}, '{quoted}');\n"
    script += 'SELECT id, code FROM probe_result ORDER BY id;\nDROP DATABASE catalog;\n'
    codes = {}
    for index, code in mysql(dialect, name, script):
        codes[int(index)] = int(code)
    return [codes[index] for index in range(len(statements))]


def call(function, count):
    return f"SELECT {function}({', '.join(['NULL'] * count)})"


def help_topics(dialect, name):
    """Function names of the help tables, their category and the calls their syntax shows."""
    rows = mysql(dialect, name, r"""
SELECT t.name, c.name, REPLACE(REPLACE(t.description, '\n', ' '), '\t', ' ')
FROM mysql.help_topic t JOIN mysql.help_category c ON c.help_category_id = t.help_category_id;
""")
    topics = {}
    for topic, category, description in rows:
        word = topic.strip().lower()
        if not re.fullmatch(r'[a-z_][a-z0-9_]*', word):
            continue
        syntax = description.split('Syntax:', 1)[-1][:600]
        topics[word] = (category, syntax)
    return topics


def overloads_from_syntax(function, syntax):
    """The parameter names of each call of `function` that a help text shows, as `NAME(a,b[,c])`."""
    found = []
    pattern = re.compile(re.escape(function) + r'\(', re.IGNORECASE)
    for match in pattern.finditer(syntax):
        depth, end = 1, match.end()
        while end < len(syntax) and depth:
            depth += {'(': 1, ')': -1}.get(syntax[end], 0)
            end += 1
        inner = syntax[match.end():end - 1]
        if re.search(r'\b(FROM|FOR|IN|AS|USING|SEPARATOR|ORDER|PASSING|COLUMNS|RETURNING)\b', inner):
            continue
        params = []
        optional = False
        for raw in re.split(r'(\[?,)', inner):
            if raw in (',', '[,'):
                optional = optional or raw == '[,'
                continue
            text = raw.strip()
            if text.startswith('['):
                optional = True
            text = re.sub(r'[\[\]]', '', text).strip()
            text = re.sub(r'^DISTINCT\s+', '', text, flags=re.IGNORECASE)
            if not text:
                continue
            if text in ('...', '…'):
                if params:
                    params[-1] = '...' + params[-1].lstrip('.')
                continue
            variadic = text.endswith('...')
            text = text.rstrip('. ')
            if not re.fullmatch(r'[A-Za-z_][A-Za-z0-9_]*', text) or len(text) > 30:
                params = None
                break
            entry = text.lower() + ':'
            if variadic:
                entry = '...' + entry
            if optional:
                entry += '?'
            params.append(entry)
        if params is not None:
            found.append(params)
    return found


def signatures_of(accepted, most, shown):
    """The overloads of a function from the numbers of arguments the server takes (up to `most`,
    which stands for any number) and the calls its help text shows. Overloads that only add
    arguments at the end become one with optional parameters."""
    shown = [[entry.rstrip('?').lstrip('.') for entry in params] for params in shown]

    def names(count):
        for params in shown:
            if len(params) >= count:
                return params[:count]
        return [f'arg{index + 1}:' for index in range(count)]

    if accepted[-1] == most:
        fewest = accepted[0]
        params = names(fewest + 1)
        params[-1] = '...' + params[-1]
        return [single_expression(params)]
    signatures = []
    for count in accepted:
        params = names(count)
        previous = signatures[-1] if signatures else None
        if previous is not None and len(previous) == count - 1 and [p.rstrip('?') for p in previous] == params[:-1]:
            previous.append(params[-1] + '?')
        else:
            signatures.append(params)
    return [single_expression(params) for params in signatures]


def single_expression(params):
    """`arg1` alone reads better as `expr`."""
    return [entry.replace('arg1:', 'expr:') for entry in params] if len(params) == 1 else params


INTERNAL = re.compile(r'^(internal_|can_access_|get_dd_|is_visible_dd_object$)')


def mysql_catalog(dialect, name, candidates):
    topics = help_topics(dialect, name)
    names = sorted(set(candidates) | set(topics) | set(GRAMMAR_FUNCTIONS))
    if dialect == 'mariadb':
        names = sorted(set(names) | {row[0].lower() for row in mysql(dialect, name, 'SELECT FUNCTION FROM information_schema.SQL_FUNCTIONS;')})
    names = [function for function in names if re.fullmatch(r'[a-z_][a-z0-9_]*', function)]
    counts = range(0, 7)
    statements = [call(function, count) for function in names for count in counts]
    codes = probe(dialect, name, statements)
    # A window function is only read with OVER after it.
    windowed = probe(dialect, name, [statement + ' OVER ()' for statement in statements])
    window_functions = set()
    for position, function in enumerate(names):
        span = slice(position * len(counts), (position + 1) * len(counts))
        if all(code == SYNTAX for code in codes[span]) and any(code != SYNTAX for code in windowed[span]):
            codes[span] = windowed[span]
            window_functions.add(function)
    functions = []
    for position, function in enumerate(names):
        results = codes[position * len(counts):(position + 1) * len(counts)]
        if all(code in UNKNOWN_FUNCTION or code == SYNTAX for code in results):
            continue
        if any(code in UNKNOWN_FUNCTION for code in results):
            continue
        accepted = [count for count, code in zip(counts, results) if code not in (WRONG_COUNT, SYNTAX)]
        if not accepted:
            continue
        category, syntax = topics.get(function, ('', ''))
        if INTERNAL.match(function) or category == 'Internal Functions':
            continue
        kind = 'f'
        if re.search(r'aggregate|group by', category, re.IGNORECASE):
            kind = 'a'
        elif re.search(r'window', category, re.IGNORECASE) or function in window_functions:
            kind = 'w'
        for params in signatures_of(accepted, counts[-1], overloads_from_syntax(function, syntax)):
            functions.append((function, kind, ','.join(params), ''))
    statements = [f'CREATE TABLE probe_type (c {type_name})' for type_name in MYSQL_TYPES]
    types = [
        (re.sub(r'\(.*\)', '', type_name), category)
        for (type_name, category), code in zip(MYSQL_TYPES.items(), probe(dialect, name, statements))
        if code == 0
    ]
    schemas = "('information_schema', 'mysql', 'performance_schema', 'sys')"
    relations = [
        (schema.lower(), relation.lower(), 'view' if 'VIEW' in kind else 'table')
        for schema, relation, kind in mysql(dialect, name, f"""
SELECT table_schema, table_name, table_type FROM information_schema.tables WHERE table_schema IN {schemas};
""")
    ]
    columns = [
        (schema.lower(), relation.lower(), column.lower(), column_type)
        for schema, relation, column, column_type in mysql(dialect, name, f"""
SELECT table_schema, table_name, column_name, column_type FROM information_schema.columns
WHERE table_schema IN {schemas} ORDER BY table_schema, table_name, ordinal_position;
""")
    ]
    if dialect == 'mariadb':
        variables = mysql(dialect, name, 'SELECT LOWER(VARIABLE_NAME), LOWER(VARIABLE_TYPE) FROM information_schema.SYSTEM_VARIABLES;')
    else:
        variables = [
            (row[0].lower(), '')
            for row in mysql(dialect, name, """
SELECT VARIABLE_NAME FROM performance_schema.global_variables
UNION SELECT VARIABLE_NAME FROM performance_schema.session_variables;
""")
        ]
    return functions, types, relations, columns, [tuple(variable) for variable in variables]


# SQLite

SQLITE_SCRIPT = r"""
.mode tabs
SELECT 'F', name, type, narg FROM pragma_function_list;
SELECT 'P', name FROM pragma_pragma_list;
SELECT 'C', 'main', 'sqlite_schema', name, type FROM pragma_table_info('sqlite_schema');
SELECT 'C', 'temp', 'sqlite_temp_schema', name, type FROM pragma_table_info('sqlite_temp_schema');
CREATE TABLE seq (id INTEGER PRIMARY KEY AUTOINCREMENT);
CREATE TABLE stat (id INTEGER PRIMARY KEY);
CREATE INDEX stat_id ON stat (id);
ANALYZE;
SELECT 'C', 'main', 'sqlite_sequence', name, type FROM pragma_table_info('sqlite_sequence');
SELECT 'C', 'main', 'sqlite_stat1', name, type FROM pragma_table_info('sqlite_stat1');
"""


# Functions of the shell's own extensions and of compile options a library seldom has.
SQLITE_SHELL_FUNCTIONS = re.compile(
    r'^(base64|base85|is_base85|decimal.*|dtostr|edit|ieee754.*|lsmode|readfile|realpath|regexpi?|sha1.*|sha3.*'
    r'|shell_.*|sqlite_offset|stmtrand|strtod|unknown|usleep|writefile|geopoly_.*|median|percentile.*)$'
)


def sqlite_catalog(image):
    result = docker('run', '--rm', '-i', image, 'sh', '-c', 'apk add -q sqlite >/dev/null 2>&1 && sqlite3 :memory:', stdin=SQLITE_SCRIPT)
    if result.returncode != 0:
        raise RuntimeError(result.stderr)
    functions, pragmas, columns = [], [], []
    arities = {}
    for line in result.stdout.splitlines():
        fields = line.split('\t')
        if fields[0] == 'F':
            _, function, kind, narg = fields
            if not re.fullmatch(r'[a-z_][a-z0-9_]*', function) or SQLITE_SHELL_FUNCTIONS.match(function):
                continue
            arities.setdefault((function, {'s': 'f', 'a': 'a', 'w': 'w'}.get(kind, 'f')), set()).add(int(narg))
        elif fields[0] == 'P':
            pragmas.append((fields[1], ''))
        elif fields[0] == 'C':
            columns.append(tuple(fields[1:]))
    for (function, kind), counts in sorted(arities.items()):
        most = 7
        if any(count < 0 for count in counts):
            fixed = sorted(count for count in counts if count >= 0)
            accepted = list(range(fixed[0] if fixed else 1, most + 1))
        else:
            accepted = sorted(counts)
        for params in signatures_of(accepted, most, []):
            functions.append((function, kind, ','.join(params), ''))
    relations = sorted({(schema, relation, 'table') for schema, relation, _, _ in columns})
    alias_columns = [('main', 'sqlite_master', column, kind) for schema, relation, column, kind in columns if relation == 'sqlite_schema']
    alias_columns += [('temp', 'sqlite_temp_master', column, kind) for schema, relation, column, kind in columns if relation == 'sqlite_temp_schema']
    relations += [('main', 'sqlite_master', 'table'), ('temp', 'sqlite_temp_master', 'table')]
    return functions, list(SQLITE_TYPES.items()), relations, columns + alias_columns, pragmas


# Writing


def version_range(present, sampled):
    """`*`, `since-`, `-until` or `since-until` over the versions sampled, in order."""
    first = sampled.index(present[0])
    last = sampled.index(present[-1])
    since = '' if first == 0 else sampled[first]
    until = '' if last == len(sampled) - 1 else sampled[last]
    if not since and not until:
        return '*'
    return f'{since}-{until}'


def merge(per_version, sampled):
    """Each row with the versions it was seen in, in the order first seen."""
    seen = {}
    for version in sampled:
        for row in per_version[version]:
            seen.setdefault(row, [])
            if version not in seen[row]:
                seen[row].append(version)
    return [(row, version_range(versions, sampled)) for row, versions in seen.items()]


def write(dialect, sampled, catalogs):
    lines = [
        f'# The built-in catalog of {dialect}, taken from the servers by scripts/catalog.py. Do not edit.',
        f'# Versions sampled: {", ".join(sampled)}.',
    ]
    parts = {kind: {version: catalogs[version][index] for version in sampled} for index, kind in enumerate('FTRCV')}
    for kind in 'FTRCV':
        rows = merge(parts[kind], sampled)
        if kind in 'FTV':
            rows.sort(key=lambda item: item[0])
        for row, versions in rows:
            lines.append('\t'.join([kind, *[str(field) for field in row], versions]))
    path = DATA / f'{dialect}.tsv'
    path.write_text('\n'.join(lines) + '\n')
    print(f'{path.relative_to(ROOT)}: {len(lines) - 2} lines')


def main():
    parser = argparse.ArgumentParser(description=__doc__.split('\n')[0])
    parser.add_argument('--keep', action='store_true', help='leave the containers running')
    args = parser.parse_args()
    DATA.mkdir(parents=True, exist_ok=True)
    started = start_containers()
    try:
        postgres = {version: postgres_catalog(name) for version, name, _ in SERVERS['postgres']}
        write('postgres', [version for version, _, _ in SERVERS['postgres']], postgres)
        candidates = described_names() | {row[0] for catalog in postgres.values() for row in catalog[0]}
        for dialect in ('mysql', 'mariadb'):
            catalogs = {version: mysql_catalog(dialect, name, candidates) for version, name, _ in SERVERS[dialect]}
            write(dialect, [version for version, _, _ in SERVERS[dialect]], catalogs)
        sqlite = {version: sqlite_catalog(image) for version, image in SQLITE}
        write('sqlite', [version for version, _ in SQLITE], sqlite)
    finally:
        if not args.keep:
            for name in started:
                docker('stop', name)


if __name__ == '__main__':
    main()
