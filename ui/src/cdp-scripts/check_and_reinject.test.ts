import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { loadCheckAndReinject, loadClickListener } from "./loader";

type CdpDocument = Document & {
    __cw_clicks?: unknown[];
    __cw_listener?: EventListener;
    __cw_handler?: EventListener;
};

function cdpDoc(): CdpDocument {
    return document as CdpDocument;
}

function resetCdpState() {
    const d = cdpDoc();
    if (d.__cw_listener) {
        d.removeEventListener("click", d.__cw_listener, true);
    }
    delete d.__cw_clicks;
    delete d.__cw_listener;
    delete d.__cw_handler;
    document.body.innerHTML = "";
}

describe("CDP check_and_reinject.js", () => {
    beforeEach(() => {
        resetCdpState();
    });

    afterEach(() => {
        resetCdpState();
    });

    it("returns 'alive' and leaves state untouched when the listener exists", () => {
        loadClickListener()();
        const queueBefore = cdpDoc().__cw_clicks;
        const listenerBefore = cdpDoc().__cw_listener;

        const result = loadCheckAndReinject()();

        expect(result).toBe("alive");
        expect(cdpDoc().__cw_clicks).toBe(queueBefore);
        expect(cdpDoc().__cw_listener).toBe(listenerBefore);
    });

    it("returns 'reinjected' and installs a new listener when none exists", () => {
        expect(cdpDoc().__cw_listener).toBeUndefined();

        const result = loadCheckAndReinject()();

        expect(result).toBe("reinjected");
        expect(Array.isArray(cdpDoc().__cw_clicks)).toBe(true);
        expect(typeof cdpDoc().__cw_listener).toBe("function");
    });

    it("the re-injected listener captures clicks like the original", () => {
        document.body.innerHTML = `<button id="b" aria-label="Reinjected">Hi</button>`;
        const result = loadCheckAndReinject()();
        expect(result).toBe("reinjected");

        document.getElementById("b")!.click();

        const clicks = cdpDoc().__cw_clicks as Array<{ ariaLabel: string | null }>;
        expect(clicks).toHaveLength(1);
        expect(clicks[0].ariaLabel).toBe("Reinjected");
    });
});
