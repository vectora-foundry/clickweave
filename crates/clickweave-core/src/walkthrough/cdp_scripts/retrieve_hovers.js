() => {
  if (!Array.isArray(document.__cw_hovers)) return [];
  const h = document.__cw_hovers;
  document.__cw_hovers = [];
  return h;
}
