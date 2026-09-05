"""Portable parser/guard units. These do not execute Linux cancellation or native Pliego."""

from dataclasses import replace
import unittest
from unittest.mock import patch

import check_production_cancellation as proof


def row(pid=12, parent=10, start=100, comm="Script#1", user=20, system=2):
    return proof.Stat(pid, comm, "R", parent, user, system, start)


class CancellationGuards(unittest.TestCase):
    def test_stat_original_positions_and_parentheses_in_name(self):
        fields = ["R", "10"] + ["0"] * 9 + ["321", "22"] + ["0"] * 6 + ["98765"] + ["0"] * 3
        parsed = proof.parse_stat("12 (Script#x) tricky) " + " ".join(fields))
        self.assertEqual(parsed, row(start=98765, comm="Script#x) tricky", user=321, system=22))

    def test_stat_rejects_truncation_invalid_identity_and_oversized_record(self):
        for raw in ["12 Script R 10", "12 (Script) R 10", "x (Script) " + " ".join(["0"] * 20), "a" * 4097]:
            with self.subTest(raw=raw[:40]), self.assertRaises((RuntimeError, ValueError)):
                proof.parse_stat(raw)
        raw = "0 (Script) R 10 " + " ".join(["0"] * 17 + ["10"])
        with self.assertRaises(RuntimeError):
            proof.parse_stat(raw)

    def test_lineage_requires_complete_owned_parent_chain(self):
        rows = {10: row(10, 1, 20), 12: row(12, 10, 30), 14: row(14, 12, 40), 99: row(99, 1, 90)}
        self.assertEqual(proof.lineage(rows, 14, (10, 20)), [(14, 40), (12, 30), (10, 20)])
        self.assertEqual(proof.lineage(rows, 99, (10, 20)), [])
        self.assertEqual(proof.lineage(rows, 10, (10, 20)), [])
        self.assertEqual(proof.lineage(rows, 14, (10, 21)), [])
        del rows[12]
        self.assertEqual(proof.lineage(rows, 14, (10, 20)), [])

    def test_lineage_rejects_cycles_and_reused_parent_pid(self):
        self.assertEqual(proof.lineage({12: row(12, 14), 14: row(14, 12)}, 12, (10, 20)), [])
        self.assertEqual(proof.lineage({10: row(10, 1, 200), 12: row(12, 10, 100)}, 12, (10, 200)), [])

    def test_only_exact_native_render_and_owned_runtime_cwd_match(self):
        root = "/fresh/php/cancel-jobs"
        meta = {
            "exe_identity": [4, 5],
            "argv": ["/bin/pliego", "render-api2"],
            "cwd": root + "/" + "a" * 32 + "/runtime",
        }
        self.assertTrue(proof.is_cancel_render(meta, (4, 5), root))
        for overrides in [
            {"exe_identity": [4, 6]},
            {"argv": ["/bin/pliego", "--contract-probe"]},
            {"argv": ["sh", "/bin/pliego", "render-api2"]},
            {"argv": ["/bin/pliego", "render-api2", "extra"]},
            {"cwd": "/fresh/php/preflight-jobs/" + "a" * 32 + "/runtime"},
            {"cwd": root + "-other/" + "a" * 32 + "/runtime"},
            {"cwd": root + "/../../other/runtime"},
            {"cwd": root + "/not-a-job/runtime"},
            {"cwd": root + "/" + "a" * 32 + "/runtime/child"},
        ]:
            with self.subTest(overrides=overrides):
                self.assertFalse(proof.is_cancel_render({**meta, **overrides}, (4, 5), root))

    def test_cpu_requires_same_named_thread_identity_and_positive_work(self):
        first = row()
        self.assertTrue(proof.script_cpu_growth(first, replace(first, user_ticks=35), 15))
        for last in [
            first,
            replace(first, user_ticks=34),
            replace(first, pid=99, user_ticks=50),
            replace(first, start_ticks=101, user_ticks=50),
            replace(first, comm="Worker", user_ticks=50),
        ]:
            self.assertFalse(proof.script_cpu_growth(first, last, 15))
        worker = replace(first, comm="Worker")
        self.assertFalse(proof.script_cpu_growth(worker, replace(worker, user_ticks=50), 15))

    def test_pidfd_acquisition_rejects_reused_pid_and_closes_handle(self):
        for current, exited in [(replace(row(), start_ticks=999), False), (row(), True)]:
            with (
                self.subTest(current=current, exited=exited),
                patch.object(proof.os, "pidfd_open", return_value=77, create=True),
                patch.object(proof, "read_stat", return_value=current),
                patch.object(proof, "metadata", return_value={}),
                patch.object(proof, "terminated", return_value=exited),
                patch.object(proof.os, "close") as close,
            ):
                with self.assertRaises(ProcessLookupError):
                    proof.pin(row(), [(12, 100), (10, 50)])
                close.assert_called_once_with(77)

    def test_pidfd_acquisition_preserves_original_ancestry(self):
        chain = [(12, 100), (10, 50)]
        with (
            patch.object(proof.os, "pidfd_open", return_value=77, create=True),
            patch.object(proof, "read_stat", return_value=row()),
            patch.object(proof, "metadata", return_value={"test": True}),
            patch.object(proof, "terminated", return_value=False),
        ):
            pinned = proof.pin(row(), chain)
        self.assertEqual(pinned.stat.identity, (12, 100))
        self.assertEqual(pinned.ancestry, chain)
        self.assertEqual(pinned.fd, 77)

    def test_observed_reparented_native_descendant_stays_accounted(self):
        instance = object.__new__(proof.Proof)
        native = proof.Pinned(row(), 1, [(12, 100), (10, 50)], {})
        child = proof.Pinned(row(14, 12, 110), 2, [(14, 110), (12, 100), (10, 50)], {})
        unrelated = proof.Pinned(row(99, 10, 120), 3, [(99, 120), (10, 50)], {})
        instance.owned = {p.stat.identity: p for p in [native, child, unrelated]}
        rows = {12: row(), 14: row(14, 1, 110), 99: row(99, 10, 120)}
        self.assertEqual(set(instance.native_members(native, rows)), {(12, 100), (14, 110)})

    def test_reused_pid_does_not_reclassify_old_unrelated_identity(self):
        instance = object.__new__(proof.Proof)
        native = proof.Pinned(row(), 1, [(12, 100), (10, 50)], {})
        old = proof.Pinned(row(99, 10, 80), 2, [(99, 80), (10, 50)], {})
        instance.owned = {p.stat.identity: p for p in [native, old]}
        rows = {12: row(), 99: row(99, 12, 120)}
        self.assertEqual(set(instance.native_members(native, rows)), {(12, 100)})

    def test_signal_uses_only_pinned_handle_and_skips_exited_process(self):
        instance = object.__new__(proof.Proof)
        process = proof.Pinned(row(), 77, [(12, 100), (10, 50)], {})
        with (
            patch.object(proof, "terminated", return_value=False),
            patch.object(proof.signal, "pidfd_send_signal", create=True) as send,
            patch.object(proof.Proof, "event"),
        ):
            instance.signal(process, 9, "test")
            send.assert_called_once_with(77, 9)
        with (
            patch.object(proof, "terminated", return_value=True),
            patch.object(proof.signal, "pidfd_send_signal", create=True) as send,
        ):
            instance.signal(process, 9, "test")
            send.assert_not_called()


if __name__ == "__main__":
    unittest.main()
