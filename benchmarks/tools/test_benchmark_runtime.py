"""Exact browser adapter allowlist; not a basename or arbitrary PHP classifier."""

import unittest

from benchmark_runtime import BROWSERSHOT_TARGET, GENERIC_TARGET, runtime_target


class AppRuntimeClassificationTests(unittest.TestCase):
    def test_exact_render_entrypoints(self) -> None:
        for adapter in ("browsershot", "invobook-browsershot"):
            path = f"/repo/benchmarks/adapters/{adapter}/adapter.php"
            self.assertEqual(runtime_target([path, "render", "input.html"]), BROWSERSHOT_TARGET)
            self.assertEqual(runtime_target([path, "identity"]), GENERIC_TARGET)

    def test_similar_paths_and_indirect_commands_remain_generic(self) -> None:
        for command in (
            ["/tmp/invobook-browsershot/adapter.php", "render"],
            ["/repo/benchmarks/adapters/invobook-browsershot-evil/adapter.php", "render"],
            ["/repo/benchmarks/adapters/aureus-dompdf/adapter.php", "render"],
            ["php", "/repo/benchmarks/adapters/invobook-browsershot/adapter.php", "render"],
            ["/repo/benchmarks/adapters/invobook-browsershot/adapter.php", "--config", "render"],
        ):
            self.assertEqual(runtime_target(command), GENERIC_TARGET)


if __name__ == "__main__":
    unittest.main()
