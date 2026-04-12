// Load each CDP injected-JS script via Vite's `?raw` import and compose it
// the same way Rust does at compile time, so vitest runs the exact bytes
// that ship to the browser.

import commonSrc from "../../../crates/clickweave-core/src/walkthrough/cdp_scripts/common.js?raw";
import clickListenerSrc from "../../../crates/clickweave-core/src/walkthrough/cdp_scripts/click_listener.js?raw";
import checkAndReinjectSrc from "../../../crates/clickweave-core/src/walkthrough/cdp_scripts/check_and_reinject.js?raw";
import hoverListenerSrc from "../../../crates/clickweave-core/src/walkthrough/cdp_scripts/hover_listener.js?raw";
import retrieveClickSrc from "../../../crates/clickweave-core/src/walkthrough/cdp_scripts/retrieve_click.js?raw";
import retrieveHoversSrc from "../../../crates/clickweave-core/src/walkthrough/cdp_scripts/retrieve_hovers.js?raw";
import stopHoverSrc from "../../../crates/clickweave-core/src/walkthrough/cdp_scripts/stop_hover.js?raw";

function composeWithCommon(bodySrc: string): () => unknown {
    const src = `() => {\n${commonSrc}${bodySrc}}`;
    return new Function(`return (${src});`)() as () => unknown;
}

function composeStandalone(src: string): () => unknown {
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
