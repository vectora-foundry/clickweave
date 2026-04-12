// Shared DOM-global shape and reset helpers for CDP injected-JS tests.
// The injected scripts stash state on `document` under `__cw_*` keys — tests
// narrow or widen as needed, but reset logic is identical across the suite.

export type CdpDocument = Document & {
    __cw_clicks?: unknown[];
    __cw_listener?: EventListener;
    __cw_handler?: EventListener;
    __cw_hovers?: unknown[];
    __cw_hover_interval?: ReturnType<typeof setInterval> | null;
    __cw_hover_mousemove?: EventListener | null;
    __cw_hover_flush?: (() => void) | null;
    __cw_hover_lastEl?: Element | null;
    __cw_hover_enterTime?: number;
    __cw_hover_cx?: number;
    __cw_hover_cy?: number;
};

export function cdpDoc(): CdpDocument {
    return document as CdpDocument;
}

export function resetClickState(): void {
    const d = cdpDoc();
    if (d.__cw_listener) {
        d.removeEventListener("click", d.__cw_listener, true);
    }
    delete d.__cw_clicks;
    delete d.__cw_listener;
    delete d.__cw_handler;
    document.body.innerHTML = "";
}

export function resetHoverState(): void {
    const d = cdpDoc();
    if (d.__cw_hover_interval) {
        clearInterval(d.__cw_hover_interval);
    }
    if (d.__cw_hover_mousemove) {
        d.removeEventListener("mousemove", d.__cw_hover_mousemove, true);
    }
    delete d.__cw_hovers;
    delete d.__cw_hover_interval;
    delete d.__cw_hover_mousemove;
    delete d.__cw_hover_flush;
    delete d.__cw_hover_lastEl;
    delete d.__cw_hover_enterTime;
    delete d.__cw_hover_cx;
    delete d.__cw_hover_cy;
    document.body.innerHTML = "";
}
