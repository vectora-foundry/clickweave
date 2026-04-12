import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { createListenerRegistry } from "./RecordingBarView";

describe("createListenerRegistry", () => {
  let consoleErrorSpy: ReturnType<typeof vi.spyOn>;

  beforeEach(() => {
    consoleErrorSpy = vi.spyOn(console, "error").mockImplementation(() => {});
  });

  afterEach(() => {
    consoleErrorSpy.mockRestore();
  });

  it("invokes every tracked unlistener when disposed after all registrations resolve", async () => {
    const { subscribe, dispose } = createListenerRegistry();
    const unlistenA = vi.fn();
    const unlistenB = vi.fn();

    await Promise.all([
      subscribe(Promise.resolve(unlistenA)),
      subscribe(Promise.resolve(unlistenB)),
    ]);
    dispose();

    expect(unlistenA).toHaveBeenCalledTimes(1);
    expect(unlistenB).toHaveBeenCalledTimes(1);
  });

  it("still disposes sibling listeners when one subscription rejects", async () => {
    const { subscribe, dispose } = createListenerRegistry();
    const unlistenA = vi.fn();

    await Promise.all([
      subscribe(Promise.resolve(unlistenA)),
      subscribe(Promise.reject(new Error("listen failed"))),
    ]);
    dispose();

    expect(unlistenA).toHaveBeenCalledTimes(1);
    expect(consoleErrorSpy).toHaveBeenCalled();
  });

  it("releases listeners that resolve after dispose has already run", async () => {
    const { subscribe, dispose } = createListenerRegistry();
    const unlistenLate = vi.fn();
    let resolveLate: (u: () => void) => void = () => {};
    const latePromise = new Promise<() => void>((resolve) => {
      resolveLate = resolve;
    });

    const pending = subscribe(latePromise);
    dispose();
    resolveLate(unlistenLate);
    await pending;

    expect(unlistenLate).toHaveBeenCalledTimes(1);
  });
});
