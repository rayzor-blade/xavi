#!/usr/bin/env python3
"""Collect pinned Cargo license notices and unmodified MPL source archives."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tarfile
import tempfile
import urllib.error
import urllib.request
from concurrent.futures import ThreadPoolExecutor


def fetch(url):
    request = urllib.request.Request(url, headers={'User-Agent': 'hxn-release-notices'})
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            return response.read()
    except urllib.error.HTTPError as error:
        if error.code == 403 and url.startswith('https://api.github.com/') and shutil.which('gh'):
            return subprocess.check_output(['gh', 'api', url.removeprefix('https://api.github.com/')])
        raise


def metadata(manifest):
    # Audit the published graph, without checkout-local development patches.
    return json.loads(subprocess.check_output(['cargo', 'metadata', '--locked', '--format-version', '1', '--manifest-path', str(manifest.resolve())], cwd=tempfile.gettempdir()))


def reachable(data, roots):
    nodes = {n['id']: n for n in data['resolve']['nodes']}
    pending, seen = list(roots), set()
    while pending:
        ident = pending.pop()
        if ident in seen:
            continue
        seen.add(ident)
        pending.extend(d['pkg'] for d in nodes[ident]['deps'] if any(k['kind'] != 'dev' for k in d['dep_kinds']))
    return [p for p in data['packages'] if p['id'] in seen and p['id'] not in data['workspace_members']]


def is_notice(name):
    # Modules such as copying.rs implement APIs; they are not license texts.
    return Path(name).suffix.lower() not in ('.rs', '.c', '.h', '.cpp', '.hx', '.py', '.js', '.ts') and bool(re.match(r'^(licen[sc]e|notice|copying|copyright)([.\-_]|$)', name, re.I))


def upstream(package, root):
    vcs = root / '.cargo_vcs_info.json'
    repo = package.get('repository')
    source = package.get('source') or ''
    if not repo and source.startswith('git+'):
        repo = source[4:].split('?')[0].split('#')[0]
    if not repo or 'github.com/' not in repo:
        return []
    repo = repo.split('github.com/', 1)[1].removesuffix('.git').rstrip('/')
    revision = json.loads(vcs.read_text())['git']['sha1'] if vcs.exists() else source.rsplit('#', 1)[-1]
    if not re.fullmatch(r'[a-f0-9]{7,40}', revision):
        return []
    listing = json.loads(fetch(f'https://api.github.com/repos/{repo}/contents?ref={revision}'))
    return [(entry['name'], fetch(entry['download_url'])) for entry in listing if entry['type'] == 'file' and is_notice(entry['name'])]


def material(package, output, apache, mit, previous, existing):
    root = Path(package['manifest_path']).parent
    destination = output / 'licenses' / package.get('_notice_name', f"{package['name']}-{package['version']}")
    destination.mkdir(parents=True)
    files = [(str(p.relative_to(root)), p.read_bytes()) for p in root.rglob('*') if p.is_file() and not p.is_symlink() and not any(part in ('.git', 'target') for part in p.relative_to(root).parts) and is_notice(p.name)]
    if not files and package.get('source', '').startswith('git+'):
        for parent in root.parents:
            if (parent / '.git').exists():
                files = [(p.name, p.read_bytes()) for p in parent.iterdir() if p.is_file() and is_notice(p.name)]
                break
    if not files:
        old = previous.get((package['name'], package['version'], package.get('source')))
        if old and old['license'] == package['license']:
            files = [(str(Path(name).relative_to(Path('licenses') / destination.name)), (existing / name).read_bytes()) for name in old['files'] if is_notice(Path(name).name) or Path(name).name == 'UPSTREAM-MANIFEST.txt']
        if not files and package.get('license'):
            files = upstream(package, root)
    license_id = package.get('license') or ''
    if not files:
        if not license_id:
            files = [('UPSTREAM-MANIFEST.txt', Path(package['manifest_path']).read_bytes())]
        elif license_id == 'CC0-1.0':
            files = [('LICENSE-CC0', fetch('https://raw.githubusercontent.com/spdx/license-list-data/v3.27.0/text/CC0-1.0.txt'))]
        elif 'Apache-2.0' in license_id and 'AND' not in license_id:
            files = [('LICENSE-APACHE', apache)]
        elif license_id == 'MIT':
            # Upstream declares MIT but supplies no separate copyright notice.
            # Preserve that declaration without inventing a copyright holder.
            files = [('LICENSE-MIT', mit), ('UPSTREAM-MANIFEST.txt', Path(package['manifest_path']).read_bytes())]
        else:
            raise ValueError(f"No upstream license text: {package['name']} {license_id}")
    if files == [('LICENSE-MIT', mit)]:
        files.append(('UPSTREAM-MANIFEST.txt', Path(package['manifest_path']).read_bytes()))
    for name, content in files:
        (destination / name).parent.mkdir(parents=True, exist_ok=True)
        (destination / name).write_bytes(content)
    record = {key: package.get(key) for key in ['name', 'version', 'license', 'repository', 'source']}
    if not license_id:
        record['license'] = 'NOASSERTION'
    record['files'] = [str((destination / name).relative_to(output)) for name, _ in files]
    if license_id == 'MPL-2.0':
        filename = f"{package['name']}-{package['version']}.crate"
        cargo = Path(os.environ.get('CARGO_HOME', str(Path.home() / '.cargo')))
        matches = list((cargo / 'registry/cache').glob(f'*/{filename}'))
        archive = matches[0].read_bytes() if matches else fetch(f"https://static.crates.io/crates/{package['name']}/{filename}")
        (destination / 'source.crate').write_bytes(archive)
        with tarfile.open(destination / 'source.crate', 'r:gz') as tar:
            assert any(n.endswith('/src/lib.rs') for n in tar.getnames())
        record['source_archive'] = str((destination / 'source.crate').relative_to(output))
        record['source_sha256'] = hashlib.sha256(archive).hexdigest()
    return record


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--manifest', type=Path, default=Path('Cargo.toml'))
    parser.add_argument('--package', action='append', default=[])
    parser.add_argument('--extra-manifest', type=Path)
    parser.add_argument('--extra-package', action='append', default=[])
    parser.add_argument('--output', type=Path, default=Path('third-party'))
    parser.add_argument('--check', action='store_true')
    args = parser.parse_args()
    if args.check:
        inventory = json.loads((args.output / 'inventory.json').read_text())
        base = args.manifest.resolve().parent
        for item in inventory['inputs']:
            assert hashlib.sha256((base / item['path']).read_text(encoding='utf-8').encode()).hexdigest() == item['sha256'], f"Refresh notices after changing {item['path']}"
        for item in inventory['packages']:
            for name in item['files']:
                assert (args.output / name).stat().st_size > 0, name
            if item.get('source_archive'):
                assert hashlib.sha256((args.output / item['source_archive']).read_bytes()).hexdigest() == item['source_sha256']
        unresolved = [f"{p['name']} {p['version']}" for p in inventory['packages'] if p.get('license') == 'NOASSERTION']
        assert not unresolved, f"Resolve upstream licenses before releasing: {', '.join(unresolved)}"
        print('Pinned dependency notices and source archives are current.')
        return
    data = metadata(args.manifest)
    roots = [p['id'] for p in data['packages'] if p['name'] in args.package] if args.package else data['workspace_members']
    packages = reachable(data, roots)
    if args.extra_manifest:
        extra = metadata(args.extra_manifest)
        packages += reachable(extra, [p['id'] for p in extra['packages'] if p['name'] in args.extra_package])
    packages = list({p['id']: p for p in packages}.values())
    for package in packages:
        if sum(p['name'] == package['name'] and p['version'] == package['version'] for p in packages) > 1:
            revision = hashlib.sha256(package['source'].encode()).hexdigest()[:8]
            package['_notice_name'] = f"{package['name']}-{package['version']}-{revision}"
    apache_package = next(p for p in packages if (Path(p['manifest_path']).parent / 'LICENSE-APACHE').exists())
    apache = (Path(apache_package['manifest_path']).parent / 'LICENSE-APACHE').read_bytes()
    # The standard permission/warranty terms, without inventing copyright data.
    mit_package = next(p for p in packages if (Path(p['manifest_path']).parent / 'LICENSE-MIT').exists())
    mit = (Path(mit_package['manifest_path']).parent / 'LICENSE-MIT').read_text()
    mit = re.sub(r'^Copyright[^\n]*\n', '', mit, flags=re.M).encode()
    output = args.output.resolve()
    temporary = output.with_name(output.name + '.new')
    if temporary.exists():
        raise FileExistsError(f'Remove the previous incomplete notice output: {temporary}')
    temporary.mkdir(parents=True)
    previous = {}
    if (output / 'inventory.json').exists():
        old = json.loads((output / 'inventory.json').read_text())
        previous = {(p['name'], p['version'], p.get('source')): p for p in old['packages']}
    with ThreadPoolExecutor(max_workers=6) as pool:
        records = list(pool.map(lambda p: material(p, temporary, apache, mit, previous, output), packages))
    records.sort(key=lambda p: (p['name'], p['version']))
    base = args.manifest.resolve().parent
    lock = base / 'Cargo.lock'
    inputs = [lock, *[Path(p['manifest_path']) for p in data['packages'] if p['id'] in data['workspace_members']]]
    inventory = {'inputs': [{'path': str(p.relative_to(base)), 'sha256': hashlib.sha256(p.read_text(encoding='utf-8').encode()).hexdigest()} for p in sorted(set(inputs))], 'packages': records}
    (temporary / 'inventory.json').write_text(json.dumps(inventory, indent=2) + '\n')
    lines = [
        '# Third-party notices', '',
        'This distribution includes code from the dependencies listed below.',
        'The inventory includes platform-specific and build dependencies; not every',
        'dependency is linked into every artifact. Original license and notice texts',
        'are retained under `licenses/`. These terms apply to their respective code,',
        'and do not change the license of this project.', '',
        'When redistributing binaries, retain the applicable notices and license texts.', '',
    ]
    for p in records:
        if p['license'] == 'NOASSERTION':
            lines += [f"**Unresolved license:** {p['name']} {p['version']} has no declared license at its pinned source revision. Its manifest is retained for provenance, not as a license grant. Resolve upstream terms before releasing this distribution.", '']
        if p.get('source_archive'):
            archive = p['source_archive']
            lines += [f"The unchanged {p['name']} source is included as [source.crate]({archive}) under MPL-2.0. Preserve source availability and its notices when redistributing.", '']
    lines += ['| Dependency | Declared license | License and notices |', '|---|---|---|']
    for p in records:
        links = ', '.join(f'[{Path(f).name}]({f})' for f in p['files'])
        lines.append(f"| {p['name']} {p['version']} | {p['license']} | {links} |")
    (temporary / 'NOTICES.md').write_text('\n'.join(lines) + '\n')
    if output.exists():
        assert (output / 'inventory.json').is_file(), 'Refusing to replace a directory not generated by this tool'
        shutil.rmtree(output)
    temporary.rename(output)
    print(f'{len(records)} dependency license records written to {output}')


if __name__ == '__main__':
    main()
