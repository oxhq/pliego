/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

(() => {
    if (Object.prototype.hasOwnProperty.call(window, "__pliegoReadiness")) {
        return;
    }

    let timer;
    let state = Object.freeze({ status: "pending" });
    document.documentElement?.classList.add("test-wait");

    const settle = next => {
        if (state.status !== "pending") {
            return false;
        }
        state = Object.freeze(next);
        clearTimeout(timer);
        document.documentElement?.classList.remove("test-wait");
        return true;
    };

    const normalizeError = error => {
        const code = typeof error?.code === "string" && error.code.trim()
            ? error.code.trim()
            : "READINESS_FAILED";
        const candidate = typeof error?.message === "string" ? error.message : error;
        const message = String(candidate ?? "").trim() || "Document reported a readiness failure";
        return Object.freeze({ code, message });
    };

    const ready = Object.freeze(payload => settle({
        status: "ready",
        payload: payload === undefined ? null : payload,
    }));
    const fail = Object.freeze(error => settle({
        status: "failed",
        error: normalizeError(error),
    }));

    Object.defineProperty(window, "__pliegoReadiness", {
        configurable: false,
        enumerable: false,
        get: () => state,
    });
    Object.defineProperty(window, "pliego", {
        configurable: false,
        enumerable: true,
        writable: false,
        value: Object.freeze({ ready, fail }),
    });

    timer = setTimeout(() => fail({
        code: "READINESS_TIMEOUT",
        message: "Document readiness timed out after __PLIEGO_TIMEOUT_MS__ ms",
    }), __PLIEGO_TIMEOUT_MS__);
})();
