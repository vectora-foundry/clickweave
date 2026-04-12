import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { loadHoverListener, loadStopHover } from "./loader";
import { cdpDoc, resetHoverState } from "./test-helpers";

type HoverEntry = {
    ts: number;
    dwellMs: number;
    x: number;
    y: number;
    tagName: string;
    role: string | null;
    ariaLabel: string | null;
    textContent: string | null;
    href: string | null;
    parentRole: string | null;
    parentName: string | null;
};

describe("CDP hover_listener.js", () => {
    beforeEach(() => {
        resetHoverState();
        vi.useFakeTimers();
    });

    afterEach(() => {
        vi.useRealTimers();
        resetHoverState();
    });

    it("initializes hover state and registers mousemove + polling interval", () => {
        loadHoverListener(500)();

        const d = cdpDoc();
        expect(Array.isArray(d.__cw_hovers)).toBe(true);
        expect(d.__cw_hovers).toHaveLength(0);
        expect(d.__cw_hover_cx).toBe(0);
        expect(d.__cw_hover_cy).toBe(0);
        expect(typeof d.__cw_hover_mousemove).toBe("function");
        expect(typeof d.__cw_hover_flush).toBe("function");
        expect(d.__cw_hover_interval).toBeTruthy();
    });

    it("mousemove updates the cached cursor position", () => {
        loadHoverListener(500)();

        document.dispatchEvent(
            new MouseEvent("mousemove", { clientX: 42, clientY: 77, bubbles: true }),
        );

        const d = cdpDoc();
        expect(d.__cw_hover_cx).toBe(42);
        expect(d.__cw_hover_cy).toBe(77);
    });

    it("records a hover entry once dwell exceeds the minimum threshold", () => {
        // jsdom does not implement elementFromPoint — stub it so the polling
        // interval sees our synthetic element.
        document.body.innerHTML = `
            <button id="a" aria-label="Play">Play</button>
            <button id="b" aria-label="Next">Next</button>`;
        const a = document.getElementById("a")!;
        const b = document.getElementById("b")!;
        let current: Element = a;
        document.elementFromPoint = ((_x: number, _y: number) => current) as Document["elementFromPoint"];

        loadHoverListener(200)();

        // Poll once: pointer enters button `a` (lastEl transitions from null -> a).
        vi.advanceTimersByTime(100);
        expect(cdpDoc().__cw_hover_lastEl).toBe(a);
        expect(cdpDoc().__cw_hovers).toHaveLength(0);

        // Accumulate dwell past the 200ms threshold, then transition to `b`;
        // the transition flushes the dwell on `a`.
        vi.advanceTimersByTime(250);
        current = b;
        vi.advanceTimersByTime(100);

        const hovers = cdpDoc().__cw_hovers!;
        expect(hovers).toHaveLength(1);
        expect(hovers[0].tagName).toBe("BUTTON");
        expect(hovers[0].ariaLabel).toBe("Play");
        expect(hovers[0].textContent).toBe("Play");
        expect(hovers[0].dwellMs).toBeGreaterThanOrEqual(200);
    });

    it("drops hovers that do not meet the minimum dwell threshold", () => {
        document.body.innerHTML = `
            <button id="a">A</button>
            <button id="b">B</button>`;
        const a = document.getElementById("a")!;
        const b = document.getElementById("b")!;
        let current: Element | null = a;
        document.elementFromPoint = ((_x: number, _y: number) => current) as Document["elementFromPoint"];

        loadHoverListener(500)();

        vi.advanceTimersByTime(100);
        expect(cdpDoc().__cw_hover_lastEl).toBe(a);

        // Switch the pointer to `b` almost immediately — dwell on `a` is < 500ms.
        vi.advanceTimersByTime(100);
        current = b;
        vi.advanceTimersByTime(100);

        expect(cdpDoc().__cw_hovers).toHaveLength(0);
        expect(cdpDoc().__cw_hover_lastEl).toBe(b);
    });

    it("stop_hover.js clears the interval and the mousemove handler", () => {
        loadHoverListener(500)();
        const d = cdpDoc();
        expect(d.__cw_hover_interval).toBeTruthy();
        expect(d.__cw_hover_mousemove).toBeTruthy();

        loadStopHover()();

        expect(d.__cw_hover_interval).toBeNull();
        expect(d.__cw_hover_mousemove).toBeNull();
        expect(d.__cw_hover_flush).toBeNull();
    });

    it("re-injecting removes the previous mousemove listener (no dup captures)", () => {
        const inject = loadHoverListener(500);
        inject();
        const firstHandler = cdpDoc().__cw_hover_mousemove;
        inject();
        const secondHandler = cdpDoc().__cw_hover_mousemove;

        expect(firstHandler).not.toBe(secondHandler);

        // Dispatch one mousemove — only the latest handler should observe it.
        document.dispatchEvent(
            new MouseEvent("mousemove", { clientX: 10, clientY: 20, bubbles: true }),
        );
        expect(cdpDoc().__cw_hover_cx).toBe(10);
        expect(cdpDoc().__cw_hover_cy).toBe(20);
    });
});
