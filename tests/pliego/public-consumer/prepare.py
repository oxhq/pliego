# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

"""Prepare a fresh public-only consumer or exact same-app upgrade/lock rollback.

Does not run Composer, install a runtime, or contact a network.
"""
import argparse
import hashlib
import json
from pathlib import Path


def require(ok, message):
    if not ok:
        raise ValueError(message)


def manifest(framework, version):
    return {'name': 'pliego-proof/public-consumer', 'type': 'project', 'license': 'proprietary',
            'description': 'Isolated public Pliego package install and rollback proof',
            'require': {'php': '^8.4', 'laravel/framework': framework, 'oxhq/pliego-laravel': version},
            'config': {'allow-plugins': False, 'preferred-install': 'dist', 'source-fallback': False,
                       'secure-http': True}}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('stage', choices=['initial', 'upgrade', 'rollback', 'fresh-12'])
    parser.add_argument('application', type=Path)
    args = parser.parse_args()
    app = args.application
    if args.stage in ('initial', 'fresh-12'):
        require(not app.exists() and not app.is_symlink(), 'Fresh application path is required')
        app.mkdir(parents=False)
        for name in ('composer-home', 'composer-cache', 'public-identity', 'evidence', 'rollback-lock'):
            (app / name).mkdir()
        document = manifest('13.30.1', '0.3.3') if args.stage == 'initial' else manifest('12.69.1', '0.4.0')
        (app / 'composer.json').write_text(json.dumps(document, indent=2) + '\n', encoding='utf-8')
    else:
        require(app.is_dir() and not app.is_symlink(), 'Existing real consumer is required')
        prior = 'initial' if args.stage == 'upgrade' else 'upgrade'
        report = json.loads((app / 'evidence' / prior / 'report.json').read_bytes())
        require(report['outcome'] == 'passed' and report['stage'] == prior, 'Previous public stage is not qualified')
        pdf = json.loads((app / 'evidence' / prior / 'independent-pdf.json').read_bytes())
        require(pdf['outcome'] == 'passed', 'Previous independent PDF check is missing')
        identity = json.loads((app / 'public-identity' / prior / 'identity.json').read_bytes())
        require(hashlib.sha256((app / 'composer.lock').read_bytes()).hexdigest() == identity['lock_sha256'],
                'Resolved lock changed after previous public identity check')
        target = app / 'composer.json'
        backup = app / 'rollback-lock'
        if args.stage == 'upgrade':
            require(not (backup / 'composer.json').exists() and not (backup / 'composer.lock').exists(), 'Original lock snapshot already exists')
            document = json.loads(target.read_bytes())
            require(document == manifest('13.30.1', '0.3.3'), 'Unexpected initial manifest')
            for name in ('composer.json', 'composer.lock'):
                (backup / name).write_bytes((app / name).read_bytes())
            document['require']['oxhq/pliego-laravel'] = '0.4.0'
            target.write_text(json.dumps(document, indent=2) + '\n', encoding='utf-8')
        else:
            require(json.loads(target.read_bytes()) == manifest('13.30.1', '0.4.0'), 'Unexpected upgraded manifest')
            for name in ('composer.json', 'composer.lock'):
                (app / name).write_bytes((backup / name).read_bytes())
    print('Prepared ' + args.stage + '; no packages installed or public proof claimed.')


if __name__ == '__main__':
    main()
