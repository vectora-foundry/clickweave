  d.__cw_hovers = [];
  d.__cw_hover_cx = 0;
  d.__cw_hover_cy = 0;
  d.__cw_hover_enter_sx = 0;
  d.__cw_hover_enter_sy = 0;
  d.__cw_hover_lastEl = null;
  d.__cw_hover_enterTime = 0;
  const MIN_DWELL = __CW_MIN_DWELL__;
  if (d.__cw_hover_mousemove) {
    d.removeEventListener('mousemove', d.__cw_hover_mousemove, true);
  }
  d.__cw_hover_mousemove = (e) => {
    d.__cw_hover_cx = e.clientX;
    d.__cw_hover_cy = e.clientY;
  };
  d.addEventListener('mousemove', d.__cw_hover_mousemove, true);
  d.__cw_hover_flush = () => {
    const el = d.__cw_hover_lastEl;
    const enter = d.__cw_hover_enterTime;
    if (!el || !enter) return;
    const now = Date.now();
    if ((now - enter) < MIN_DWELL) return;
    let text = accessibleText(el);
    if (!text) text = findFallbackText(el);
    const { parentRole, parentName } = findParentRoleAndName(el);
    d.__cw_hovers.push({
      ts: enter,
      dwellMs: now - enter,
      x: d.__cw_hover_enter_sx,
      y: d.__cw_hover_enter_sy,
      tagName: el.tagName,
      role: el.getAttribute('role') || TAG_ROLES[el.tagName] || null,
      ariaLabel: el.ariaLabel || el.getAttribute('aria-label') || null,
      textContent: text || null,
      href: el.closest('a')?.href || null,
      parentRole: parentRole,
      parentName: parentName,
    });
    d.__cw_hover_lastEl = null;
    d.__cw_hover_enterTime = 0;
  };
  if (d.__cw_hover_interval) clearInterval(d.__cw_hover_interval);
  d.__cw_hover_interval = setInterval(() => {
    const raw = d.elementFromPoint(d.__cw_hover_cx, d.__cw_hover_cy);
    if (!raw) { d.__cw_hover_lastEl = null; d.__cw_hover_enterTime = 0; return; }
    const el = raw.closest(INTERACTIVE) || raw.closest('[aria-label]') || raw;
    if (el === d.__cw_hover_lastEl) return;
    d.__cw_hover_flush();
    d.__cw_hover_lastEl = el;
    d.__cw_hover_enterTime = Date.now();
    d.__cw_hover_enter_sx = d.__cw_hover_cx + window.screenX;
    d.__cw_hover_enter_sy = d.__cw_hover_cy + window.screenY;
  }, 100);
