import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { loadClickListener, loadRetrieveClick } from "./loader";
import { cdpDoc, resetClickState } from "./test-helpers";

type ClickEntry = {
    ts: number;
    tagName: string;
    role: string | null;
    ariaLabel: string | null;
    textContent: string | null;
    title: string | null;
    value: string | null;
    href: string | null;
    id: string | null;
    className: string | null;
    parentRole: string | null;
    parentName: string | null;
};

describe("CDP click_listener.js", () => {
    beforeEach(() => {
        resetClickState();
    });

    afterEach(() => {
        resetClickState();
    });

    it("initializes the click queue and installs a capture-phase listener", () => {
        loadClickListener()();

        const d = cdpDoc();
        expect(Array.isArray(d.__cw_clicks)).toBe(true);
        expect(d.__cw_clicks).toHaveLength(0);
        expect(typeof d.__cw_listener).toBe("function");
        expect(typeof d.__cw_handler).toBe("function");
    });

    it("captures aria-label as textContent when clicking a button", () => {
        document.body.innerHTML = `<button id="b" aria-label="Submit form"><span>Submit</span></button>`;
        loadClickListener()();

        document.getElementById("b")!.click();

        const clicks = cdpDoc().__cw_clicks!;
        expect(clicks).toHaveLength(1);
        expect(clicks[0].tagName).toBe("BUTTON");
        expect(clicks[0].role).toBe("button");
        expect(clicks[0].ariaLabel).toBe("Submit form");
        expect(clicks[0].textContent).toBe("Submit form");
        expect(clicks[0].id).toBe("b");
    });

    it("falls back to visible text when no aria-label is set", () => {
        document.body.innerHTML = `<button id="b">Click me</button>`;
        loadClickListener()();

        document.getElementById("b")!.click();

        const clicks = cdpDoc().__cw_clicks!;
        expect(clicks[0].textContent).toBe("Click me");
        expect(clicks[0].ariaLabel).toBeNull();
    });

    it("resolves the nearest interactive ancestor when clicking a descendant", () => {
        document.body.innerHTML = `
            <button id="b" aria-label="Parent button">
                <span id="inner"><i>icon</i></span>
            </button>`;
        loadClickListener()();

        (document.getElementById("inner")!.firstElementChild as HTMLElement).click();

        const clicks = cdpDoc().__cw_clicks!;
        expect(clicks).toHaveLength(1);
        expect(clicks[0].tagName).toBe("BUTTON");
        expect(clicks[0].ariaLabel).toBe("Parent button");
    });

    it("walks up to a labelled ancestor when no text is found locally", () => {
        // `span` with tabindex is interactive but has no visible text; ancestor
        // div carries the aria-label that findFallbackText picks up.
        document.body.innerHTML = `
            <div aria-label="Outer label">
                <span id="s" tabindex="0"></span>
            </div>`;
        loadClickListener()();

        document.getElementById("s")!.click();

        const clicks = cdpDoc().__cw_clicks!;
        expect(clicks[0].textContent).toBe("Outer label");
        expect(clicks[0].parentName).toBe("Outer label");
    });

    it("records parentRole/parentName from the nearest ancestor with role or label", () => {
        document.body.innerHTML = `
            <nav role="navigation" aria-label="Primary">
                <a id="a" href="/x">Home</a>
            </nav>`;
        loadClickListener()();

        document.getElementById("a")!.click();

        const clicks = cdpDoc().__cw_clicks!;
        expect(clicks[0].parentRole).toBe("navigation");
        expect(clicks[0].parentName).toBe("Primary");
    });

    it("re-injecting replaces the previous listener (no duplicate clicks)", () => {
        document.body.innerHTML = `<button id="b">Hi</button>`;
        const inject = loadClickListener();
        inject();
        inject();

        document.getElementById("b")!.click();

        // Exactly one entry — the first listener was removed.
        expect(cdpDoc().__cw_clicks).toHaveLength(1);
    });

    it("retrieve_click.js shifts the oldest entry off the queue", () => {
        document.body.innerHTML = `<button id="b">Hi</button>`;
        loadClickListener()();
        document.getElementById("b")!.click();
        document.getElementById("b")!.click();

        const retrieve = loadRetrieveClick();
        const first = retrieve() as ClickEntry | null;
        const second = retrieve() as ClickEntry | null;
        const third = retrieve() as ClickEntry | null;

        expect(first?.tagName).toBe("BUTTON");
        expect(second?.tagName).toBe("BUTTON");
        expect(third).toBeNull();
    });
});
