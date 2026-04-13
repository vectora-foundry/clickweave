import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { loadRetrieveHovers } from "./loader";
import { cdpDoc, type CdpHoverRecord } from "./test-helpers";

describe("CDP retrieve_hovers.js", () => {
    beforeEach(() => {
        delete cdpDoc().__cw_hovers;
    });

    afterEach(() => {
        delete cdpDoc().__cw_hovers;
    });

    it("returns an empty array when the hover queue is uninitialized", () => {
        expect(loadRetrieveHovers()()).toEqual([]);
    });

    it("returns the full queue and clears it atomically", () => {
        const entries = [{ ts: 1 }, { ts: 2 }, { ts: 3 }] as CdpHoverRecord[];
        cdpDoc().__cw_hovers = entries;

        const result = loadRetrieveHovers()();

        expect(result).toEqual(entries);
        // Retrieval must reset the live queue so subsequent calls return an
        // empty result (the injected hovers get drained once).
        expect(cdpDoc().__cw_hovers).toEqual([]);
        expect(loadRetrieveHovers()()).toEqual([]);
    });

    it("returns empty array when the queue field is not an array", () => {
        (cdpDoc() as unknown as Record<string, unknown>).__cw_hovers = { not: "an array" };
        expect(loadRetrieveHovers()()).toEqual([]);
    });
});
