# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

"""Exact browser adapter allowlist; not a basename or arbitrary PHP classifier."""

import unittest

from benchmark_runtime import (
    BROWSERSHOT_TARGET,
    GENERIC_TARGET,
    PRIVATE_RUNTIME_CONTRACT,
    FIXED_HOME_CONTRACT,
    runtime_contract,
    runtime_target,
)


class AppRuntimeClassificationTests(unittest.TestCase):
    def test_exact_render_entrypoints(self) -> None:
        for adapter in ("browsershot", "invobook-browsershot", "invobook-browsershot-laravel12"):
            path = f"/repo/benchmarks/adapters/{adapter}/adapter.php"
            self.assertEqual(runtime_target([path, "render", "input.html"]), BROWSERSHOT_TARGET)
            self.assertEqual(runtime_contract(runtime_target([path, "render", "input.html"])), PRIVATE_RUNTIME_CONTRACT)
            self.assertEqual(runtime_target([path, "identity"]), GENERIC_TARGET)
            self.assertEqual(runtime_contract(runtime_target([path, "identity"])), FIXED_HOME_CONTRACT)

    def test_similar_paths_and_indirect_commands_remain_generic(self) -> None:
        for command in (
            ["/tmp/invobook-browsershot/adapter.php", "render"],
            ["/repo/benchmarks/adapters/invobook-browsershot-evil/adapter.php", "render"],
            ["/tmp/invobook-browsershot-laravel12/adapter.php", "render"],
            ["/repo/benchmarks/adapters/invobook-browsershot-laravel12-evil/adapter.php", "render"],
            ["/repo/benchmarks/adapters/invobook-browsershot-laravel12/adapter.php.bad", "render"],
            ["/repo/benchmarks/adapters/aureus-dompdf/adapter.php", "render"],
            ["php", "/repo/benchmarks/adapters/invobook-browsershot/adapter.php", "render"],
            ["/repo/benchmarks/adapters/invobook-browsershot/adapter.php", "--config", "render"],
            ["php", "/repo/benchmarks/adapters/invobook-browsershot-laravel12/adapter.php", "render"],
            ["/repo/benchmarks/adapters/invobook-browsershot-laravel12/adapter.php", "--config", "render"],
        ):
            self.assertEqual(runtime_target(command), GENERIC_TARGET)


if __name__ == "__main__":
    unittest.main()
