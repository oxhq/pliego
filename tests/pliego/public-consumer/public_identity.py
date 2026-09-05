# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

"""Verify installed public Composer dist bytes and native publication before execution.

Run only after explicit authorization and coordinated publication. This helper
does not install dependencies, extract archives, execute code, or accept credentials.
"""
from __future__ import annotations

import argparse
import hashlib
import io
import json
import os
from pathlib import Path, PurePosixPath
import re
import stat
import urllib.request
import zipfile


def require(ok, message):
    if not ok:
        raise ValueError(message)


def sha(data):
    return hashlib.sha256(data).hexdigest()


def verify_dist(data, installed):
    """Compare every safe ZIP file to installed bytes without extracting anything."""
    entries = {}
    with zipfile.ZipFile(io.BytesIO(data)) as archive:
        require(len(archive.infolist()) <= 10000
                and sum(item.file_size for item in archive.infolist()) <= 64 * 1024 * 1024,
                'Public dist expands beyond its bound')
        roots = set()
        for item in archive.infolist():
            path = PurePosixPath(item.filename)
            require(not path.is_absolute() and all(p not in ('', '.', '..') and ':' not in p
                    for p in item.filename.rstrip('/').split('/'))
                    and '\\' not in item.filename, 'Unsafe public dist path')
            require(not stat.S_ISLNK(item.external_attr >> 16), 'Linked public dist entry')
            roots.add(path.parts[0])
            if item.is_dir():
                continue
            require(len(path.parts) >= 2, 'Dist file outside package root')
            relative = '/'.join(path.parts[1:])
            require(relative not in entries and item.file_size <= 16 * 1024 * 1024, 'Duplicate or oversized dist file')
            target = installed.joinpath(*path.parts[1:])
            require(target.is_file() and not target.is_symlink()
                    and target.resolve().is_relative_to(installed.resolve()), 'Missing or linked installed dist file')
            payload = archive.read(item)
            require(target.read_bytes() == payload, 'Installed bytes differ from public dist: ' + relative)
            entries[relative] = sha(payload)
        require(len(roots) == 1 and bool(entries), 'Wrong dist root or empty public package')
    actual = {str(p.relative_to(installed)).replace('\\', '/') for p in installed.rglob('*') if p.is_file()}
    require(actual == set(entries), 'Installed package has additional/missing files')
    return entries


def get(url, destination, limit=16 * 1024 * 1024):
    require(url.startswith('https://'), 'HTTPS required')
    request = urllib.request.Request(url, headers={'User-Agent': 'Pliego-public-consumer-proof'})
    with urllib.request.urlopen(request, timeout=60) as response:
        require(response.url.startswith('https://'), 'Non-HTTPS redirect')
        body = response.read(limit + 1)
    require(len(body) <= limit, 'Public response exceeds bound')
    destination.write_bytes(body)
    return body


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--app', type=Path, required=True)
    parser.add_argument('--version', choices=['0.3.3', '0.4.0'], required=True)
    parser.add_argument('--native-source', required=True)
    parser.add_argument('--php-source', required=True)
    parser.add_argument('--laravel-source', required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    for key in ('COMPOSER_AUTH', 'GH_TOKEN', 'GITHUB_TOKEN', 'PLIEGO_BINARY', 'PLIEGO_TEST_AUTOLOAD'):
        require(not os.environ.get(key), 'Clear ' + key)
    for value in (args.native_source, args.php_source, args.laravel_source):
        require(re.fullmatch('[0-9a-f]{40}', value), 'Expected exact reviewed commit SHA')
    app = args.app.resolve(strict=True)
    require(app.is_dir() and not args.app.is_symlink(), 'Unsafe application')
    args.output.mkdir(parents=False, exist_ok=False)
    lock_bytes = (app / 'composer.lock').read_bytes()
    lock = json.loads(lock_bytes)
    installed = json.loads((app / 'vendor/composer/installed.json').read_bytes())['packages']
    composer = json.loads((app / 'composer.json').read_bytes())
    require('repositories' not in composer, 'No repository/path overrides in public consumer')
    report = {'schema': 'pliego.final-public-package-identity.v1', 'application': str(app),
              'version': args.version, 'native_source_commit': args.native_source,
              'lock_sha256': sha(lock_bytes), 'packages': {}, 'helper_sha256': sha(Path(__file__).read_bytes())}
    (args.output / 'composer.lock').write_bytes(lock_bytes)
    for name, expected in (('pliego-php', args.php_source), ('pliego-laravel', args.laravel_source)):
        metadata = json.loads(get('https://packagist.org/packages/oxhq/' + name + '.json', args.output / (name + '-packagist.json')))
        versions = metadata['package']['versions']
        public = versions.get('v' + args.version, versions.get(args.version))
        require(isinstance(public, dict), 'Exact public version is unavailable: ' + name)
        packages = [p for p in lock['packages'] if p['name'] == 'oxhq/' + name]
        records = [p for p in installed if p['name'] == 'oxhq/' + name]
        require(len(packages) == len(records) == 1, 'Missing/duplicate package identity')
        package, record = packages[0], records[0]
        for p in (public, package, record):
            require(p['version'].removeprefix('v') == args.version
                    and p['source']['reference'] == p['dist']['reference'] == expected, 'Wrong public/source/dist reference')
            require(p['dist']['type'] == 'zip' and p['dist']['url'] == public['dist']['url'], 'Wrong public dist selection')
        require(record['installation-source'] == 'dist', 'Source fallback is not public dist proof')
        url = public['dist']['url']
        require(url == f'https://api.github.com/repos/oxhq/{name}/zipball/{expected}', 'Unexpected public dist origin')
        data = get(url, args.output / (name + '.zip'))
        inventory = verify_dist(data, app / 'vendor/oxhq' / name)
        report['packages'][name] = {'reference': expected, 'dist_url': url, 'dist_sha256': sha(data), 'files': inventory}
    release = json.loads(get('https://api.github.com/repos/oxhq/pliego/releases/tags/v' + args.version, args.output / 'native-release.json'))
    require(release['draft'] is False and release['prerelease'] is False
            and release['tag_name'] == 'v' + args.version, 'Native release is not public stable')
    commit = json.loads(get('https://api.github.com/repos/oxhq/pliego/commits/v' + args.version, args.output / 'native-commit.json'))
    require(commit['sha'] == args.native_source, 'Public native tag points to another source')
    url = 'https://github.com/oxhq/pliego/releases/download/v' + args.version + '/runtimes.json'
    require(sum(a['browser_download_url'] == url for a in release['assets']) == 1, 'Missing/duplicate runtime manifest asset')
    manifest_bytes = get(url, args.output / 'runtimes.json')
    manifest = json.loads(manifest_bytes)
    require(manifest['release_ready'] is True and type(manifest['schema']) is int and manifest['schema'] == 1
            and type(manifest['api']) is int and manifest['api'] == 2
            and manifest['version'] == args.version, 'Native manifest is pending or wrong version')
    require(manifest_bytes == (app / 'vendor/oxhq/pliego-laravel/resources/runtimes.json').read_bytes(), 'Packaged manifest differs from exact public native asset')
    report['runtime_manifest_sha256'] = sha(manifest_bytes)
    report['native_release_id'] = release['id']
    report['github_immutable'] = release.get('immutable')
    (args.output / 'identity.json').write_text(json.dumps(report, indent=2) + '\n', encoding='utf-8')
    print('Verified exact public distributions and release metadata; native consumer execution remains next.')


if __name__ == '__main__':
    main()
