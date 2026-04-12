import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { loadRetrieveClick } from "./loader";
import { cdpDoc } from "./test-helpers";

describe("CDP retrieve_click.js", () => {
    beforeEach(() => {
        delete cdpDoc().__cw_clicks;
    });

    afterEach(() => {
        delete cdpDoc().__cw_clicks;
    });

    it("returns null when the click queue has never been initialized", () => {
        const retrieve = loadRetrieveClick();
        expect(retrieve()).toBeNull();
    });

    it("returns null when the click queue is an empty array", () => {
        cdpDoc().__cw_clicks = [];
        const retrieve = loadRetrieveClick();
        expect(retrieve()).toBeNull();
    });

    it("FIFO-shifts entries: first call returns the oldest entry", () => {
        cdpDoc().__cw_clicks = [
            { ts: 1, tagName: "FIRST" },
            { ts: 2, tagName: "SECOND" },
        ];
        const retrieve = loadRetrieveClick();
        expect(retrieve()).toMatchObject({ tagName: "FIRST" });
        expect(retrieve()).toMatchObject({ tagName: "SECOND" });
        expect(retrieve()).toBeNull();
    });

    it("mutates the queue in place (shift semantics)", () => {
        cdpDoc().__cw_clicks = [{ ts: 1 }, { ts: 2 }];
        loadRetrieveClick()();
        expect(cdpDoc().__cw_clicks).toHaveLength(1);
    });

    it("returns null when the queue is not an array (e.g. has been clobbered)", () => {
        (cdpDoc() as unknown as Record<string, unknown>).__cw_clicks = { bogus: true };
        const retrieve = loadRetrieveClick();
        expect(retrieve()).toBeNull();
    });
});
