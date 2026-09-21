#!/usr/bin/env python3

# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

"""Canonical benchmark-target runtime environment classification."""

from pathlib import PurePosixPath
from typing import Sequence

BROWSERSHOT_ADAPTER_SUFFIX = ("benchmarks", "adapters", "browsershot", "adapter.php")
BROWSERSHOT_TARGET = "browsershot-adapter-v1"
GENERIC_TARGET = "generic-benchmark-engine-v1"
PRIVATE_RUNTIME_CONTRACT = "fresh-private-home-xdg-v1"
FIXED_HOME_CONTRACT = "fixed-account-home-v1"


def runtime_target(argv: Sequence[str]) -> str:
    if len(argv) >= 2 and argv[1] == "render":
        parts = PurePosixPath(argv[0]).parts
        if tuple(parts[-len(BROWSERSHOT_ADAPTER_SUFFIX) :]) == BROWSERSHOT_ADAPTER_SUFFIX:
            return BROWSERSHOT_TARGET
    return GENERIC_TARGET


def runtime_contract(target: str) -> str:
    return PRIVATE_RUNTIME_CONTRACT if target == BROWSERSHOT_TARGET else FIXED_HOME_CONTRACT
