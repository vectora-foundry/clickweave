import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { loadHoverListener, loadStopHover } from "./loader";
import { cdpDoc, resetHoverState } from "./test-helpers";

type HoverEntry = {
    ts: number;
    dwellMs: number;
    tagName: string;
    textContent: string | null;
};

describe("CDP stop_hover.js", () => {
    beforeEach(() => {
        resetHoverState();
        vi.useFakeTimers();
    });

    afterEach(() => {
        vi.useRealTimers();
        resetHoverState();
    });

    it("is a no-op when no hover listener was ever installed", () => {
        // None of the `__cw_hover_*` fields exist — stop_hover must not throw.
        expect(() => loadStopHover()()).not.toThrow();
    });

    it("flushes a pending hover that has exceeded the dwell threshold", () => {
        document.body.innerHTML = `<button id="b" aria-label="Stopper">Stop</button>`;
        const btn = document.getElementById("b")!;
        document.elementFromPoint = ((_x: number, _y: number) => btn) as Document["elementFromPoint"];

        loadHoverListener(100)();

        // Poll once: pointer enters the button.
        vi.advanceTimersByTime(100);
        expect(cdpDoc().__cw_hover_lastEl).toBe(btn);

        // Accumulate dwell beyond the 100ms threshold, then stop.
        vi.advanceTimersByTime(200);

        loadStopHover()();

        const hovers = cdpDoc().__cw_hovers!;
        expect(hovers).toHaveLength(1);
        expect(hovers[0].tagName).toBe("BUTTON");
        expect(hovers[0].textContent).toBe("Stopper");
        expect(cdpDoc().__cw_hover_interval).toBeNull();
        expect(cdpDoc().__cw_hover_mousemove).toBeNull();
        expect(cdpDoc().__cw_hover_flush).toBeNull();
    });

    it("does not flush a hover that is below the dwell threshold", () => {
        document.body.innerHTML = `<button id="b">Short</button>`;
        const btn = document.getElementById("b")!;
        document.elementFromPoint = ((_x: number, _y: number) => btn) as Document["elementFromPoint"];

        loadHoverListener(500)();
        vi.advanceTimersByTime(100); // pointer enters btn
        vi.advanceTimersByTime(100); // only 100ms of dwell

        loadStopHover()();

        expect(cdpDoc().__cw_hovers).toHaveLength(0);
    });
});
