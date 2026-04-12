  d.__cw_clicks = [];
  d.__cw_handler = (e) => {
    const el = e.target.closest(INTERACTIVE) || e.target.closest('[aria-label]') || e.target;
    let text = accessibleText(el);
    if (!text) text = findFallbackText(el);
    const { parentRole, parentName } = findParentRoleAndName(el);
    d.__cw_clicks.push({
      ts: Date.now(),
      tagName: el.tagName,
      role: el.getAttribute('role') || TAG_ROLES[el.tagName] || null,
      ariaLabel: el.ariaLabel || el.getAttribute('aria-label') || null,
      textContent: text || null,
      title: el.title || el.closest('[title]')?.title || null,
      value: el.value || null,
      href: el.closest('a')?.href || null,
      id: el.id || null,
      className: el.className || null,
      parentRole: parentRole,
      parentName: parentName,
    });
  };
  if (d.__cw_listener) {
    d.removeEventListener('click', d.__cw_listener, true);
  }
  d.__cw_listener = (e) => d.__cw_handler(e);
  d.addEventListener('click', d.__cw_listener, true);
