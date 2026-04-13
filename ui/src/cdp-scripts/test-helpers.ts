// Shared DOM-global shape and reset helpers for CDP injected-JS tests.
// The injected scripts stash state on `document` under `__cw_*` keys — tests
// narrow or widen as needed, but reset logic is identical across the suite.

// Shapes mirror the records pushed by the injected listeners in
// crates/clickweave-core/src/walkthrough/cdp_scripts/{click,hover}_listener.js.
export type CdpClickRecord = {
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

export type CdpHoverRecord = {
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

export type CdpDocument = Document & {
    __cw_clicks?: CdpClickRecord[];
    __cw_listener?: EventListener;
    __cw_handler?: EventListener;
    __cw_hovers?: CdpHoverRecord[];
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
