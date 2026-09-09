# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

import io
import hashlib
import json
import os
from pathlib import Path
import sys
import tempfile
import unittest
import zipfile
from unittest.mock import patch

import prepare
from prepare import manifest
from public_identity import verify_dist
import run


class RecipeTest(unittest.TestCase):
    def package(self, values):
        output = io.BytesIO()
        with zipfile.ZipFile(output, 'w') as archive:
            for name, payload in values.items():
                archive.writestr(name, payload)
        return output.getvalue()

    def test_no_path_repository_or_source_fallback(self):
        for version in ('0.3.3', '0.4.0'):
            value = manifest('13.30.1', version)
            self.assertNotIn('repositories', value)
            self.assertFalse(value['config']['source-fallback'])
            self.assertFalse(value['config']['allow-plugins'])
            self.assertEqual(value['require']['oxhq/pliego-laravel'], version)

    def test_exact_public_dist_and_negative_bytes_paths_inventory(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / 'VERSION').write_bytes(b'0.4.0\n')
            good = self.package({'root/VERSION': b'0.4.0\n'})
            self.assertEqual(set(verify_dist(good, root)), {'VERSION'})
            for data in (self.package({'root/VERSION': b'0.3.3\n'}),
                         self.package({'root/../VERSION': b'0.4.0\n'}),
                         self.package({'root/VERSION': b'0.4.0\n', 'another/LICENSE': b'x'})):
                with self.assertRaises(ValueError):
                    verify_dist(data, root)
            (root / 'extra.php').write_bytes(b'not from the dist')
            with self.assertRaises(ValueError):
                verify_dist(good, root)

    def test_preparation_requires_stage_evidence_and_restores_exact_lock(self):
        with tempfile.TemporaryDirectory() as temporary:
            app = Path(temporary) / 'isolated'
            def run(stage):
                with patch('sys.argv', ['prepare.py', stage, str(app)]):
                    prepare.main()
            run('initial')
            initial = (app / 'composer.json').read_bytes()
            lock = b'{"packages": []}\n'
            (app / 'composer.lock').write_bytes(lock)
            with self.assertRaises(ValueError):
                run('initial')
            with self.assertRaises(FileNotFoundError):
                run('upgrade')
            for stage in ('initial', 'upgrade'):
                proof = app / 'evidence' / stage
                proof.mkdir()
                (proof / 'report.json').write_text(json.dumps({'outcome': 'passed', 'stage': stage}))
                (proof / 'independent-pdf.json').write_text('{"outcome": "passed"}')
                public = app / 'public-identity' / stage
                public.mkdir()
                (public / 'identity.json').write_text(json.dumps({'lock_sha256': hashlib.sha256(lock).hexdigest()}))
                run('upgrade' if stage == 'initial' else 'rollback')
            self.assertEqual((app / 'composer.json').read_bytes(), initial)
            self.assertEqual((app / 'composer.lock').read_bytes(), lock)

    def test_rejection_report_uses_typed_kind_not_nonexistent_code(self):
        consumer = Path(__file__).with_name('consumer.php').read_text(encoding='utf-8')
        self.assertNotRegex(consumer, r"\$error->result\s*\[\s*['\"]error['\"]\s*\]\s*\[\s*['\"]code['\"]\s*\]")
        self.assertIn("demand($error->kind === 'resource'", consumer)
        self.assertIn("'kind' => $error->kind", consumer)
        self.assertIn("retain($proof.'/rejected-result.json', $error->result)", consumer)
        self.assertIn("JobRetention::STATUS_FILE", consumer)
        self.assertIn("'success_delivery_absent' => true", consumer)

    def test_exact_source_and_isolated_credential_free_environment(self):
        self.assertEqual(run.source('a' * 40), 'a' * 40)
        for bad in ('a' * 39, 'a' * 41, 'A' * 40, 'b' * 7, 'refs/tags/v0.4.0', None):
            with self.assertRaises(ValueError):
                run.source(bad)
        with patch.dict(os.environ, {name: 'not-public' for name in run.CREDENTIALS}):
            env = run.environment(Path('isolated-app'))
            self.assertTrue(all(name not in env for name in run.CREDENTIALS))
            self.assertEqual(env['COMPOSER_HOME'], str(Path('isolated-app/composer-home')))
            self.assertEqual(os.environ['PLIEGO_TEST_AUTOLOAD'], 'not-public')

    def test_upgrade_cannot_change_application_dependencies(self):
        before = {'packages': [{'name': name, 'version': '0.3.3'} for name in sorted(run.SDK_PACKAGES)]
                  + [{'name': 'laravel/framework', 'version': '13.30.1'}], 'packages-dev': []}
        after = json.loads(json.dumps(before))
        for package in after['packages'][:2]:
            package['version'] = '0.4.0'
        self.assertEqual(run.upgrade_diff(before, after), sorted(run.SDK_PACKAGES))
        for bad in ('host-version', 'extra-dependency', 'duplicate', 'dev-dependency'):
            changed = json.loads(json.dumps(after))
            if bad == 'host-version':
                changed['packages'][2]['version'] = '13.30.2'
            elif bad == 'extra-dependency':
                changed['packages'].append({'name': 'another/package', 'version': '1'})
            elif bad == 'duplicate':
                changed['packages'].append(dict(changed['packages'][0]))
            else:
                changed['packages-dev'] = [{'name': 'another/dev'}]
            with self.assertRaises(ValueError):
                run.upgrade_diff(before, changed)

    def test_failed_command_preserves_both_streams_and_exit_before_stopping(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            with self.assertRaises(ValueError):
                run.execute([sys.executable, '-c',
                             'import sys; print("output"); print("failure", file=sys.stderr); sys.exit(7)'],
                            directory, 'failure', dict(os.environ))
            self.assertEqual((directory / 'failure.stdout.txt').read_text().strip(), 'output')
            self.assertEqual((directory / 'failure.stderr.txt').read_text().strip(), 'failure')
            record = json.loads((directory / 'failure.process.json').read_bytes())
            self.assertEqual(record['outcome'], 'completed')
            self.assertEqual(record['exit_code'], 7)

    def test_workflow_is_manual_only_and_uploads_positive_evidence_inventory(self):
        root = Path(__file__).resolve().parents[3]
        workflow = (root / '.github/workflows/pliego-public-consumer.yml').read_text()
        self.assertIn('  workflow_dispatch:', workflow)
        self.assertNotIn('\n  push:', workflow)
        self.assertNotIn('\n  pull_request:', workflow)
        self.assertIn('    timeout-minutes: 25', workflow)
        self.assertIn('        timeout-minutes: 20', workflow)
        self.assertIn('          persist-credentials: false', workflow)
        self.assertIn('        if: always()', workflow)
        self.assertNotIn('PUBLIC_CONSUMER_PARENT: ${{ runner.temp }}', workflow)
        self.assertIn('Join-Path $env:RUNNER_TEMP', workflow)
        self.assertIn('Add-Content -LiteralPath $env:GITHUB_ENV', workflow)
        inventory = workflow.split('          path: |\n', 1)[1]
        self.assertNotIn('/vendor', inventory)
        self.assertNotIn('composer-home', inventory)
        self.assertNotIn('composer-cache', inventory)
        self.assertNotIn('/pliego-runtime', inventory)
        self.assertIn('/public-identity/', inventory)
        self.assertIn('/rollback-lock/', inventory)


if __name__ == '__main__':
    unittest.main()
