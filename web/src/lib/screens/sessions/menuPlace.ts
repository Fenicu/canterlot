/** Меню целиком на экране: под строкой, иначе над ней, иначе поверх неё у нижнего края списка. */
export function menuTop(place: { full: number; height: number; rowTop: number; rowBottom: number }): number {
  const { full, height, rowTop, rowBottom } = place;
  if (full <= height - rowBottom) return rowBottom;
  if (full <= rowTop) return rowTop - full;
  return Math.max(0, height - full);
}
