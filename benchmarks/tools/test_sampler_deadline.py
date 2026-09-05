#!/usr/bin/env python3
"""Portable deadline/cleanup guard units; Linux process execution remains a live gate."""

from __future__ import annotations

import json
import math
import os
import unittest
from types import SimpleNamespace
from unittest import mock

# The production sampler is Linux-only. Its import-time flag is unused by
# these portable mocked/pure units; do not change production launch flags.
with mock.patch.object(os, "O_CLOEXEC", getattr(os, "O_CLOEXEC", 0), create=True):
    import process_tree_sampler as sampler


class RootDeadlineTests(unittest.TestCase):
    def test_disabled_preserves_sample_poll_boundary(self) -> None:
        self.assertIsNone(sampler.root_wall_deadline(12.0, None))
        self.assertEqual(sampler.root_wait_timeout_ms(12.0, 12.075, None), math.ceil(0.075 * 1000))
        sampler.check_root_wall_deadline(1e20, None, 12.0, (123, 456))

    def test_deadline_preempts_a_later_sample(self) -> None:
        deadline = sampler.root_wall_deadline(10.0, 10.0)
        self.assertAlmostEqual(deadline, 10.01)
        self.assertEqual(sampler.root_wait_timeout_ms(10.0, 11.0, deadline), 10)
        self.assertEqual(sampler.root_wait_timeout_ms(10.02, 11.0, deadline), 0)
        self.assertEqual(sampler.root_wait_timeout_ms(10.0, 10.005, 11.0), 6)

    def test_typed_expiry_has_owned_identity_and_never_returns_a_sample(self) -> None:
        sampler.check_root_wall_deadline(10.009, 10.01, 10.0, (123, 456))
        for now in (10.01, 11.0):
            with self.assertRaises(sampler.MeasurementIncomplete) as failure:
                sampler.check_root_wall_deadline(now, 10.01, 10.0, (123, 456))
            self.assertEqual(failure.exception.code, "ROOT_WALL_TIMEOUT")
            self.assertIn("PID 123 start_ticks=456", str(failure.exception))
            self.assertIn("10.000 ms", str(failure.exception))

    def test_invalid_limits_fail_before_resource_creation(self) -> None:
        for value in (0.0, -1.0, float("nan"), float("inf"), -float("inf")):
            with self.subTest(value=value), mock.patch.object(sampler, "require_broker_root") as broker:
                with self.assertRaises(sampler.MeasurementIncomplete) as failure:
                    sampler.sample_command([], "", "", "", 75, 250, 1000, 10000, None, root_wall_timeout_ms=value)
                self.assertEqual(failure.exception.code, "ROOT_WALL_TIMEOUT_INVALID")
                broker.assert_not_called()

    def test_timeout_cleanup_is_bound_to_unreaped_measurement_child(self) -> None:
        parent = SimpleNamespace(close=mock.Mock())
        child = SimpleNamespace(close=mock.Mock())
        staging = SimpleNamespace(close=mock.Mock())
        original = sampler.incomplete("ROOT_WALL_TIMEOUT", "fixture deadline")
        with mock.patch.object(sampler, "force_cleanup") as cleanup:
            sampler.cleanup_failed_sample_resources(parent, child, staging, None, 123, False, True, original)
        self.assertEqual(cleanup.call_args_list, [mock.call(child, parent, 123), mock.call(staging, parent, None)])
        child.close.assert_called_once_with()
        staging.close.assert_called_once_with()
        parent.close.assert_called_once_with()

    def test_cleanup_failure_keeps_deadline_cause_and_does_not_claim_drain(self) -> None:
        parent = SimpleNamespace(close=mock.Mock())
        child = SimpleNamespace(close=mock.Mock())
        original = sampler.incomplete("ROOT_WALL_TIMEOUT", "fixture deadline")
        with mock.patch.object(
            sampler, "force_cleanup", side_effect=sampler.incomplete("CGROUP_CLEANUP_FAILED", "still populated")
        ):
            with self.assertRaises(sampler.MeasurementIncomplete) as failure:
                sampler.cleanup_failed_sample_resources(parent, child, None, None, 123, False, True, original)
        self.assertEqual(failure.exception.code, "CGROUP_CLEANUP_FAILED")
        diagnostic = json.loads(str(failure.exception).split("diagnostic=", 1)[1])
        self.assertEqual(diagnostic["original_failure"]["code"], "ROOT_WALL_TIMEOUT")
        parent.close.assert_called_once_with()


if __name__ == "__main__":
    unittest.main()
