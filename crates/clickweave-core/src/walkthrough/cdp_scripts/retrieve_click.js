() => {
  if (!Array.isArray(document.__cw_clicks)) return null;
  return document.__cw_clicks.shift() || null;
}
