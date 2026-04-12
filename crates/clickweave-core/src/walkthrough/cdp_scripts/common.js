  const d = document;
  const TAG_ROLES = {BUTTON:'button',A:'link',INPUT:'textbox',SELECT:'combobox',TEXTAREA:'textbox'};
  const INTERACTIVE = '[role="button"],[role="link"],[role="menuitem"],[role="menuitemcheckbox"],[role="menuitemradio"],[role="tab"],[role="treeitem"],[role="option"],[role="checkbox"],[role="radio"],[role="switch"],[role="textbox"],[role="combobox"],[role="searchbox"],[role="slider"],[role="spinbutton"],a,button,select,textarea,input,[tabindex]:not([tabindex="-1"])';
  function accessibleText(node) {
    const a = node.ariaLabel || node.getAttribute('aria-label');
    if (a) return a;
    const lb = node.getAttribute('aria-labelledby');
    if (lb) {
      const t = lb.split(/\s+/).map(id => document.getElementById(id)?.textContent?.trim() || '').filter(Boolean).join(' ');
      if (t) return t.substring(0, 200);
    }
    if (node.title) return node.title;
    if (node.alt) return node.alt;
    if (node.placeholder) return node.placeholder;
    if ((node.tagName === 'svg' || node.tagName === 'SVG') || (node.namespaceURI === 'http://www.w3.org/2000/svg' && node.tagName === 'svg')) {
      const st = node.querySelector('title');
      if (st?.textContent) return st.textContent.trim().substring(0, 200);
    }
    let t = '';
    for (const ch of node.childNodes) {
      if (ch.nodeType === 3) { t += ch.textContent; continue; }
      if (ch.nodeType === 1 && ch.getAttribute('aria-hidden') !== 'true') {
        const sub = accessibleText(ch);
        if (sub && t) t += ' ';
        t += sub;
      }
    }
    return t.trim().substring(0, 200);
  }
  function findFallbackText(el) {
    let p = el.parentElement;
    while (p && p !== d.documentElement) {
      const la = p.ariaLabel || p.getAttribute('aria-label');
      if (la) return la;
      const lb = p.getAttribute('aria-labelledby');
      if (lb) {
        const r = lb.split(/\s+/).map(id => document.getElementById(id)?.textContent?.trim() || '').filter(Boolean).join(' ');
        if (r) return r;
      }
      if (p.title) return p.title;
      p = p.parentElement;
    }
    return '';
  }
  function findParentRoleAndName(el) {
    let p = el.parentElement;
    while (p && p !== d.documentElement) {
      const r = p.getAttribute('role');
      const a = p.ariaLabel || p.getAttribute('aria-label');
      if (r || a) {
        return {
          parentRole: r || null,
          parentName: a || accessibleText(p).substring(0, 200) || null,
        };
      }
      p = p.parentElement;
    }
    return { parentRole: null, parentName: null };
  }
