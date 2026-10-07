#!/usr/bin/env python3
import argparse
import gzip
import hashlib
import io
import json
from pathlib import Path
import re
import subprocess
import tarfile
import zipfile


PACKAGE = Path(__file__).resolve().parent.parent
METADATA = json.loads((PACKAGE / 'native-source.json').read_text())


def write_json(path, value):
    path.write_text(json.dumps(value, indent=2) + '\n')


def third_party_contents(target):
    metadata = json.loads(subprocess.check_output(
        ['cargo', 'metadata', '--locked', '--filter-platform', target, '--format-version', '1'],
        cwd=PACKAGE, text=True,
    ))
    server = next(package['id'] for package in metadata['packages'] if package['name'] == 'sql-language-server')
    nodes = {node['id']: node for node in metadata['resolve']['nodes']}
    pending = [server]
    runtime = set()
    while pending:
        key = pending.pop()
        if key in runtime:
            continue
        runtime.add(key)
        pending.extend(dependency['pkg'] for dependency in nodes[key]['deps'] if any(kind['kind'] != 'dev' for kind in dependency['dep_kinds']))
    contents = {}
    dependencies = []
    for package in sorted(metadata['packages'], key=lambda value: (value['name'], value['version'])):
        if package['id'] not in runtime or not package['source']:
            continue
        folder = Path(package['manifest_path']).parent
        licenses = {path for path in folder.iterdir() if path.is_file() and any(word in path.name.upper() for word in ('LICENSE', 'LICENCE', 'COPYING', 'NOTICE', 'COPYRIGHT'))}
        if package['license_file']:
            licenses.add(folder / package['license_file'])
        if not licenses:
            raise ValueError(f'no upstream license file found for {package["name"]} {package["version"]}')
        dependency = {key: package[key] for key in ('name', 'version', 'license', 'source')}
        dependency['licenseFiles'] = []
        for path in sorted(licenses):
            name = f'third-party/{package["name"]}-{package["version"]}/{path.name}'
            contents[name] = path.read_bytes()
            dependency['licenseFiles'].append(name)
        dependencies.append(dependency)
    contents['third-party/dependencies.json'] = (json.dumps(dependencies, indent=2) + '\n').encode()
    contents['Cargo.lock'] = (PACKAGE / 'Cargo.lock').read_bytes()
    return contents


def check_pins():
    cargo = (PACKAGE / 'Cargo.toml').read_text()
    if f'version = "{METADATA["version"]}"' not in cargo:
        raise ValueError('native-source.json differs from the compiled source pins')


def asset(args):
    if args.tag != 'v' + METADATA['version'] and not re.fullmatch(r'v\d+\.\d+\.\d+-local', args.tag):
        raise ValueError(f'tag must be v{METADATA["version"]}, the version in native-source.json, or a -local tag')
    check_pins()
    source_revision = subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=PACKAGE, text=True).strip()
    platform = METADATA['platforms'][args.platform]
    version = subprocess.run([str(args.binary.resolve()), '--version'], capture_output=True, text=True, check=True, timeout=10)
    if version.stdout.strip() != 'sql-language-server ' + METADATA['version']:
        raise ValueError('native binary version differs from native-source.json')
    args.output.mkdir(parents=True, exist_ok=True)
    filename = f'sql-language-server-{args.tag}-{args.platform}.{platform["format"]}'
    archive = args.output / filename
    build = {**METADATA, 'buildRevision': source_revision, 'platform': args.platform, 'target': platform['target']}
    contents = {
        platform['executable']: args.binary.read_bytes(),
        'LICENSE': (PACKAGE / 'LICENSE').read_bytes(),
        'THIRD-PARTY.md': (PACKAGE / 'THIRD-PARTY.md').read_bytes(),
        'native-source.json': (json.dumps(build, indent=2) + '\n').encode(),
    }
    contents.update(third_party_contents(platform['target']))
    if platform['format'] == 'zip':
        with zipfile.ZipFile(archive, 'w', compression=zipfile.ZIP_DEFLATED) as output:
            for name, content in contents.items():
                entry = zipfile.ZipInfo(name)
                entry.create_system = 3
                entry.external_attr = (0o100755 if name == platform['executable'] else 0o100644) << 16
                entry.compress_type = zipfile.ZIP_DEFLATED
                output.writestr(entry, content)
    else:
        with archive.open('wb') as raw, gzip.GzipFile(fileobj=raw, mode='wb', mtime=0, filename='') as compressed:
            with tarfile.open(fileobj=compressed, mode='w') as output:
                for name, content in contents.items():
                    entry = tarfile.TarInfo(name)
                    entry.mode = 0o755 if name == platform['executable'] else 0o644
                    entry.size = len(content)
                    output.addfile(entry, io.BytesIO(content))
    checksum = hashlib.sha256(archive.read_bytes()).hexdigest()
    release = {
        'version': METADATA['version'],
        'sourceRevision': source_revision,
        'assets': {args.platform: {
            'url': f'https://github.com/basmilius/language-server-sql/releases/download/{args.tag}/{filename}',
            'sha256': checksum, 'format': platform['format'], 'executable': platform['executable'],
        }},
    }
    write_json(args.output / f'{args.platform}.json', release)
    (args.output / f'{filename}.sha256').write_text(f'{checksum}  {filename}\n')
    print(f'{filename}: {checksum}')


def merge(args):
    combined = None
    for platform in METADATA['platforms']:
        descriptors = list(args.input.rglob(f'{platform}.json'))
        if len(descriptors) != 1:
            raise ValueError(f'expected exactly one descriptor for {platform}')
        release = json.loads(descriptors[0].read_text())
        if release['version'] != METADATA['version']:
            raise ValueError('release descriptor differs from native-source.json pins')
        if combined is None:
            combined = {**release, 'assets': {}}
        if any(release[key] != combined[key] for key in ('version', 'sourceRevision')):
            raise ValueError('native assets must come from the same pinned source')
        item = release['assets'][platform]
        filename = item['url'].rsplit('/', 1)[-1]
        archives = list(args.input.rglob(filename))
        if len(archives) != 1 or hashlib.sha256(archives[0].read_bytes()).hexdigest() != item['sha256']:
            raise ValueError(f'archive checksum failed for {platform}')
        if item['format'] != METADATA['platforms'][platform]['format'] or item['executable'] != METADATA['platforms'][platform]['executable']:
            raise ValueError(f'archive metadata failed for {platform}')
        combined['assets'][platform] = item
    args.output.parent.mkdir(parents=True, exist_ok=True)
    write_json(args.output, combined)
    print(f'{args.output}: validated {len(combined["assets"])} platform assets')


def main():
    parser = argparse.ArgumentParser(description='Generate local native release archives and pinned installer descriptors.')
    commands = parser.add_subparsers(dest='command', required=True)
    create = commands.add_parser('asset')
    create.add_argument('--binary', type=Path, required=True)
    create.add_argument('--platform', choices=METADATA['platforms'], required=True)
    create.add_argument('--tag', required=True)
    create.add_argument('--output', type=Path, required=True)
    create.set_defaults(run=asset)
    combine = commands.add_parser('merge')
    combine.add_argument('--input', type=Path, required=True)
    combine.add_argument('--output', type=Path, required=True)
    combine.set_defaults(run=merge)
    args = parser.parse_args()
    args.run(args)


if __name__ == '__main__':
    main()
