import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import type { AmbiguityResolution } from "../store/slices/agentSlice";
import { AmbiguityResolutionCard } from "./AmbiguityResolutionCard";
import { AmbiguityResolutionModal } from "./AmbiguityResolutionModal";

// A tiny 1x1 transparent PNG, base64-encoded.
const TINY_PNG =
  "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNkYAAAAAYAAjCB0C8AAAAASUVORK5CYII=";

function makeResolution(
  overrides: Partial<AmbiguityResolution> = {},
): AmbiguityResolution {
  return {
    id: "res-1",
    nodeId: "node-1",
    target: "Save",
    chosenUid: "a2",
    reasoning: "Second Save is the primary toolbar action.",
    screenshotPath: "ambiguity_abc.png",
    screenshotBase64: TINY_PNG,
    createdAt: 1234,
    candidates: [
      {
        uid: "a1",
        snippet: '[uid="a1"] button "Save"',
        rect: { x: 10, y: 20, width: 30, height: 40 },
      },
      {
        uid: "a2",
        snippet: '[uid="a2"] button "Save"',
        rect: { x: 100, y: 200, width: 30, height: 40 },
      },
    ],
    ...overrides,
  };
}

describe("AmbiguityResolutionCard", () => {
  it("shows the target and chosen uid", () => {
    const onOpen = vi.fn();
    render(
      <AmbiguityResolutionCard resolution={makeResolution()} onOpen={onOpen} />,
    );

    expect(screen.getByText(/Ambiguity resolved/i)).toBeInTheDocument();
    expect(screen.getAllByText(/Save/).length).toBeGreaterThanOrEqual(1);
    expect(screen.getByText(/a2/)).toBeInTheDocument();
  });

  it("invokes onOpen when clicked", () => {
    const onOpen = vi.fn();
    render(
      <AmbiguityResolutionCard resolution={makeResolution()} onOpen={onOpen} />,
    );

    fireEvent.click(screen.getByTestId("ambiguity-resolution-card"));
    expect(onOpen).toHaveBeenCalledTimes(1);
  });

  it("truncates long reasoning to a preview", () => {
    const reasoning = "x".repeat(300);
    const onOpen = vi.fn();
    render(
      <AmbiguityResolutionCard
        resolution={makeResolution({ reasoning })}
        onOpen={onOpen}
      />,
    );

    // The preview must not contain all 300 characters; find text ending with ellipsis.
    const card = screen.getByTestId("ambiguity-resolution-card");
    expect(card.textContent).toContain("\u2026");
    expect((card.textContent ?? "").length).toBeLessThan(300);
  });
});

describe("AmbiguityResolutionModal", () => {
  beforeEach(() => {
    // jsdom lacks ResizeObserver — the modal reads it in a layout effect.
    if (typeof globalThis.ResizeObserver === "undefined") {
      class StubResizeObserver {
        observe() {}
        unobserve() {}
        disconnect() {}
      }
      // @ts-expect-error jsdom-only shim
      globalThis.ResizeObserver = StubResizeObserver;
    }
  });

  it("renders the screenshot and all candidate uids", () => {
    const onClose = vi.fn();
    render(
      <AmbiguityResolutionModal
        resolution={makeResolution()}
        onClose={onClose}
      />,
    );

    // Image is present and has the screenshot data uri as src.
    const img = screen.getByRole("img", {
      name: /Screenshot with candidate overlays/i,
    }) as HTMLImageElement;
    expect(img.src).toContain("data:image/png;base64,");
    expect(img.src).toContain(TINY_PNG);

    // Each candidate uid appears in the candidate list.
    // Two uids, appearing once in the list and possibly once in title.
    expect(screen.getAllByText(/uid=a1/).length).toBeGreaterThanOrEqual(1);
    expect(screen.getAllByText(/uid=a2/).length).toBeGreaterThanOrEqual(1);
    // Chosen candidate carries a "picked" badge (the header also says
    // "picked" so we assert at-least-one occurrence).
    expect(screen.getAllByText(/picked/i).length).toBeGreaterThanOrEqual(1);
  });

  it("has a canvas overlay that draws N rects for N candidates", () => {
    const onClose = vi.fn();
    const resolution = makeResolution();
    const { container } = render(
      <AmbiguityResolutionModal resolution={resolution} onClose={onClose} />,
    );

    // Trigger the image's load handler so the layout-effect runs.
    const img = container.querySelector("img");
    expect(img).not.toBeNull();
    Object.defineProperty(img, "naturalWidth", {
      configurable: true,
      value: 200,
    });
    Object.defineProperty(img, "naturalHeight", {
      configurable: true,
      value: 200,
    });
    fireEvent.load(img!);

    const canvas = container.querySelector("canvas");
    expect(canvas).not.toBeNull();
    // We can't reliably inspect canvas drawing in jsdom, but we can verify
    // the canvas element is present and aria-hidden.
    expect(canvas!.getAttribute("aria-hidden")).toBe("true");
    // The list of candidates renders exactly one <li> per candidate.
    const items = container.querySelectorAll("ul > li");
    expect(items.length).toBe(resolution.candidates.length);
  });

  it("calls onClose when the close button is clicked", () => {
    const onClose = vi.fn();
    render(
      <AmbiguityResolutionModal
        resolution={makeResolution()}
        onClose={onClose}
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: /close/i }));
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it("calls onClose when Escape is pressed", () => {
    const onClose = vi.fn();
    render(
      <AmbiguityResolutionModal
        resolution={makeResolution()}
        onClose={onClose}
      />,
    );
    fireEvent.keyDown(window, { key: "Escape" });
    expect(onClose).toHaveBeenCalledTimes(1);
  });
});
