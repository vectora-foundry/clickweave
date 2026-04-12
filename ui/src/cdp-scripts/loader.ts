// Helper that loads the CDP injected-JS scripts from the Rust crate so we
// can test their observable behavior in jsdom.
//
// The scripts live in `crates/clickweave-core/src/walkthrough/cdp_scripts/`
// and are assembled into a single evaluated function at Rust compile time
// via `concat!(include_str!(...), ...)`.  To mirror that at test time we
// import each file with Vite's `?raw` suffix and compose them the same way.

import commonSrc from "../../../crates/clickweave-core/src/walkthrough/cdp_scripts/common.js?raw";
import clickListenerSrc from "../../../crates/clickweave-core/src/walkthrough/cdp_scripts/click_listener.js?raw";
import checkAndReinjectSrc from "../../../crates/clickweave-core/src/walkthrough/cdp_scripts/check_and_reinject.js?raw";
import hoverListenerSrc from "../../../crates/clickweave-core/src/walkthrough/cdp_scripts/hover_listener.js?raw";
import retrieveClickSrc from "../../../crates/clickweave-core/src/walkthrough/cdp_scripts/retrieve_click.js?raw";
import retrieveHoversSrc from "../../../crates/clickweave-core/src/walkthrough/cdp_scripts/retrieve_hovers.js?raw";
import stopHoverSrc from "../../../crates/clickweave-core/src/walkthrough/cdp_scripts/stop_hover.js?raw";

/** Compose `common.js` + a listener body into a callable arrow function. */
function composeWithCommon(bodySrc: string): () => unknown {
    const src = `() => {\n${commonSrc}${bodySrc}}`;
    // eslint-disable-next-line no-new-func
    return new Function(`return (${src});`)() as () => unknown;
}

/** Wrap a standalone `() => {...}` script as a callable arrow function. */
function composeStandalone(src: string): () => unknown {
    // eslint-disable-next-line no-new-func
    return new Function(`return (${src});`)() as () => unknown;
}

export function loadClickListener(): () => void {
    return composeWithCommon(clickListenerSrc) as () => void;
}

export function loadCheckAndReinject(): () => string {
    return composeWithCommon(checkAndReinjectSrc) as () => string;
}

export function loadHoverListener(minDwellMs: number): () => void {
    const body = hoverListenerSrc.replace("__CW_MIN_DWELL__", String(minDwellMs));
    return composeWithCommon(body) as () => void;
}

export function loadRetrieveClick(): () => unknown {
    return composeStandalone(retrieveClickSrc);
}

export function loadRetrieveHovers(): () => unknown[] {
    return composeStandalone(retrieveHoversSrc) as () => unknown[];
}

export function loadStopHover(): () => void {
    return composeStandalone(stopHoverSrc) as () => void;
}

export const rawCommon = commonSrc;
