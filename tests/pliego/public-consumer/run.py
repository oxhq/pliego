# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

"""Fixed post-publication consumer sequence, bounded by its hosted workflow job.

This is not a general process supervisor. A nonzero command stops the sequence;
raw logs and an incomplete/failed report are evidence, not a consumer pass.
"""
import argparse
import hashlib
import importlib.metadata
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import time

from prepare import require


TOOLS = Path(__file__).resolve().parent
BASELINE = ('41c6cf0e9cf1c73f4f70eba9d413fa97063a3154',
            '496d2809d3b47e6aef6596b229a8b7f2135d35ae',
            '788bc6980b117375625b56ec93d40a60da5a3a2d')
CREDENTIALS = ('COMPOSER', 'COMPOSER_AUTH', 'COMPOSER_TOKEN', 'COMPOSER_AUTH_JSON',
               'PACKAGIST_TOKEN', 'GH_TOKEN', 'GITHUB_TOKEN', 'PLIEGO_BINARY',
               'PLIEGO_TEST_AUTOLOAD')
SDK_PACKAGES = {'oxhq/pliego-php', 'oxhq/pliego-laravel'}


def save(path, value):
    path.write_text(json.dumps(value, indent=2) + '\n', encoding='utf-8')


def source(value):
    require(isinstance(value, str) and re.fullmatch('[0-9a-f]{40}', value),
            'Exact lowercase 40-character source commit is required')
    return value


def environment(app):
    env = dict(os.environ)
    for name in CREDENTIALS:
        env.pop(name, None)
    env.update(COMPOSER_HOME=str(app / 'composer-home'),
               COMPOSER_CACHE_DIR=str(app / 'composer-cache'), COMPOSER_NO_INTERACTION='1')
    return env


def upgrade_diff(before, after):
    old = {p['name']: p for p in before['packages']}
    new = {p['name']: p for p in after['packages']}
    require(len(old) == len(before['packages']) and len(new) == len(after['packages']),
            'Duplicate locked packages')
    changed = sorted(name for name in old.keys() | new.keys() if old.get(name) != new.get(name))
    require(set(changed) == SDK_PACKAGES and old.keys() == new.keys(),
            'Upgrade changed dependencies outside the two Pliego packages')
    require(before.get('packages-dev') == after.get('packages-dev'), 'Upgrade changed dev dependencies')
    return changed


def execute(argv, directory, label, env):
    """Retain bounded-by-job command evidence; do not continue after any failure."""
    stdout = directory / (label + '.stdout.txt')
    stderr = directory / (label + '.stderr.txt')
    record = {'argv': [str(value) for value in argv], 'outcome': 'running'}
    target = directory / (label + '.process.json')
    save(target, record)
    started = time.monotonic()
    with stdout.open('xb') as out, stderr.open('xb') as err:
        process = subprocess.run(record['argv'], stdout=out, stderr=err, env=env, check=False)
    record.update(outcome='completed', exit_code=process.returncode,
                  wall_seconds=time.monotonic() - started)
    save(target, record)
    require(process.returncode == 0, label + ' failed; inspect retained stdout/stderr')
    return stdout.read_text(encoding='utf-8', errors='replace')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--php', type=Path, required=True)
    parser.add_argument('--composer', type=Path, required=True)
    parser.add_argument('--out', type=Path, required=True)
    parser.add_argument('--native-source', type=source, required=True)
    parser.add_argument('--php-source', type=source, required=True)
    parser.add_argument('--laravel-source', type=source, required=True)
    args = parser.parse_args()
    require(os.environ.get('GITHUB_ACTIONS') == 'true', 'Run only under the reviewed bounded hosted workflow')
    require(sys.version_info[:2] == (3, 12), 'CPython 3.12 is required')
    require(importlib.metadata.version('pypdf') == '6.16.2', 'Pinned pypdf 6.16.2 is required')
    require(sys.platform in ('linux', 'win32'), 'This recipe qualifies Linux/Windows x64 only')
    require(not args.out.exists() and not args.out.is_symlink(), 'Fresh evidence root required')
    php, composer = args.php.resolve(strict=True), args.composer.resolve(strict=True)
    require(php.is_file() and composer.is_file(), 'Explicit PHP executable and Composer PHAR required')
    args.out.mkdir(parents=False)
    out = args.out.resolve()
    logs = out / 'logs'
    logs.mkdir()
    app, app12 = out / 'same-app', out / 'fresh-laravel12'
    php_command = [php, '-d', 'auto_prepend_file=']
    final = (args.native_source, args.php_source, args.laravel_source)
    report = {'schema': 'pliego.public-consumer-workflow.v1', 'outcome': 'running',
              'sources': {'baseline': BASELINE, 'final': final}, 'proof_source': source(os.environ['GITHUB_SHA']),
              'workflow_sha256': hashlib.sha256((TOOLS.parents[2] / '.github/workflows/pliego-public-consumer.yml').read_bytes()).hexdigest(),
              'host': sys.platform, 'stages': {}, 'production_tests': {}, 'visual_review': 'pending',
              'boundary': 'Public installed consumers only; not adoption, performance, remote-storage or cancellation proof.'}
    save(out / 'report.json', report)
    save(out / 'source-hashes.json', {p.name: hashlib.sha256(p.read_bytes()).hexdigest()
                                    for p in sorted(TOOLS.iterdir()) if p.suffix in ('.py', '.php', '.md')})
    try:
        # A Composer version probe must not initialize the not-yet-created app.
        env = environment(out / 'tooling')
        php_info = json.loads(execute(php_command + ['-r',
            'echo json_encode(["version"=>PHP_VERSION,"binary"=>PHP_BINARY,"bits"=>PHP_INT_SIZE*8,"extensions"=>get_loaded_extensions()]);'],
            logs, 'php-version', env))
        require(php_info['version'].startswith('8.4.') and php_info['bits'] == 64, 'Actual 64-bit PHP 8.4 required')
        require({'mbstring', 'dom', 'xml', 'fileinfo', 'pdo_sqlite', 'zip', 'phar', 'curl', 'tokenizer', 'xmlwriter'}
                <= {name.lower() for name in php_info['extensions']}, 'Missing required PHP extensions')
        composer_version = execute(php_command + [composer, '--version', '--no-ansi'], logs, 'composer-version', env)
        require(re.search(r'^Composer version 2\.10\.1(?:\s|$)', composer_version), 'Composer 2.10.1 required')
        save(out / 'tools.json', {'php': php_info, 'composer': composer_version.strip(),
                                 'composer_phar_sha256': hashlib.sha256(composer.read_bytes()).hexdigest(),
                                 'python': sys.version, 'pypdf': importlib.metadata.version('pypdf')})
        for stage in ('initial', 'upgrade', 'rollback', 'fresh-12'):
            current = app12 if stage == 'fresh-12' else app
            env = environment(current)
            stage_logs = logs / stage
            stage_logs.mkdir()
            report['stages'][stage] = {'outcome': 'running', 'logs': str(stage_logs.relative_to(out))}
            save(out / 'report.json', report)
            execute([sys.executable, TOOLS / 'prepare.py', stage, current], stage_logs, 'prepare', env)
            common = php_command + [composer, '--no-interaction', '--no-plugins', '--no-scripts',
                                    '--working-dir=' + str(current)]
            if stage == 'rollback':
                install = ['install', '--prefer-dist', '--no-progress']
            elif stage == 'upgrade':
                install = ['update', 'oxhq/pliego-laravel', 'oxhq/pliego-php', '--minimal-changes', '--prefer-dist', '--no-progress']
            else:
                execute(common + ['validate', '--strict', '--no-check-all'], stage_logs, 'validate-manifest', env)
                install = ['update', '--prefer-dist', '--no-progress']
            execute(common + install, stage_logs, 'composer-resolve', env)
            execute(common + ['validate', '--strict', '--no-check-all'], stage_logs, 'validate-lock', env)
            execute(common + ['check-platform-reqs', '--format=json'], stage_logs, 'platform', env)
            execute(common + ['audit', '--locked', '--format=json'], stage_logs, 'audit', env)
            if stage == 'upgrade':
                changed = upgrade_diff(json.loads((app / 'rollback-lock/composer.lock').read_bytes()),
                                       json.loads((app / 'composer.lock').read_bytes()))
                save(out / 'upgrade-package-diff.json', {'changed_packages': changed})
            version = '0.3.3' if stage in ('initial', 'rollback') else '0.4.0'
            identities = BASELINE if version == '0.3.3' else final
            identity_dir = current / 'public-identity' / stage
            execute([sys.executable, TOOLS / 'public_identity.py', '--app', current, '--version', version,
                     '--native-source', identities[0], '--php-source', identities[1], '--laravel-source', identities[2],
                     '--output', identity_dir], stage_logs, 'public-identity', env)
            execute(php_command + [TOOLS / 'consumer.php', current, stage, identity_dir / 'identity.json'],
                    stage_logs, 'consumer', env)
            stage_report = current / 'evidence' / stage / 'report.json'
            execute([sys.executable, TOOLS / 'check_pdf.py', stage_report], stage_logs, 'independent-pdf', env)
            qualified = json.loads(stage_report.read_bytes())
            require(qualified['outcome'] == 'passed', 'Consumer report did not pass')
            public_identity = identity_dir / 'identity.json'
            independent_pdf = stage_report.parent / 'independent-pdf.json'
            report['stages'][stage] = {'outcome': 'passed', 'version': version, 'framework': qualified['framework'],
                                      'report': str(stage_report.relative_to(out)),
                                      'sha256': hashlib.sha256(stage_report.read_bytes()).hexdigest(),
                                      'public_identity': str(public_identity.relative_to(out)),
                                      'public_identity_sha256': hashlib.sha256(public_identity.read_bytes()).hexdigest(),
                                      'independent_pdf': str(independent_pdf.relative_to(out)),
                                      'independent_pdf_sha256': hashlib.sha256(independent_pdf.read_bytes()).hexdigest()}
            save(out / 'report.json', report)
            if stage == 'upgrade':
                # The only test-autoload override is the independently verified
                # public application's real vendor/autoload.php, never source.
                test_env = dict(env, PLIEGO_TEST_AUTOLOAD=str(app / 'vendor/autoload.php'))
                names = ('storage', 'queue') if sys.platform == 'linux' else ('storage',)
                for name in names:
                    test = app / 'vendor/oxhq/pliego-laravel/tests' / ('production_' + name + '_test.php')
                    target = out / ('production-' + name)
                    command = php_command + [test, qualified['binary'], target]
                    report['production_tests'][name] = {'outcome': 'running',
                        'report': str((target / 'report.json').relative_to(out)),
                        'installed_test_sha256': hashlib.sha256(test.read_bytes()).hexdigest()}
                    save(out / 'report.json', report)
                    if sys.platform == 'linux':
                        command = ['/usr/bin/timeout', '--kill-after=5s', '90s' if name == 'storage' else '120s'] + command
                    execute(command, stage_logs, 'production-' + name, test_env)
                    result = json.loads((target / 'report.json').read_bytes())
                    require(result['outcome'] == 'passed', 'Installed public production test failed')
                    report['production_tests'][name].update(outcome='passed',
                        sha256=hashlib.sha256((target / 'report.json').read_bytes()).hexdigest())
                    save(out / 'report.json', report)
        report['outcome'] = 'passed'
    except Exception as error:
        report['outcome'] = 'failed'
        report['error'] = {'class': type(error).__name__, 'message': str(error)}
        raise
    finally:
        save(out / 'report.json', report)


if __name__ == '__main__':
    main()
