() => {
  const d = document;
  if (d.__cw_hover_interval) { clearInterval(d.__cw_hover_interval); d.__cw_hover_interval = null; }
  if (d.__cw_hover_flush) { d.__cw_hover_flush(); d.__cw_hover_flush = null; }
  if (d.__cw_hover_mousemove) { d.removeEventListener('mousemove', d.__cw_hover_mousemove, true); d.__cw_hover_mousemove = null; }
}
