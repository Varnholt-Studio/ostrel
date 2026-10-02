// Visible window of a virtualised list with rows of one fixed height.

import { ListError } from "./sorted_index.js";

function checkCount(name, value, min) {
  if (!Number.isSafeInteger(value) || value < min) {
    throw new ListError("Window", `${name} must be an integer of at least ${min}`);
  }
}

/**
 * Returns the rows to render for a list of `count` rows of `rowHeight` pixels, shown in a
 * viewport of `viewportHeight` pixels scrolled to `scrollTop`, plus `overscan` rows above
 * and below. Result: { start, end, before, after }, rows start (inclusive) to end
 * (exclusive), and the pixel heights of the space above and below them. A scroll position
 * outside the list is clamped, so the window is never empty while rows exist.
 */
export function computeWindow({ count, rowHeight, viewportHeight, scrollTop, overscan }) {
  checkCount("count", count, 0);
  checkCount("rowHeight", rowHeight, 1);
  checkCount("viewportHeight", viewportHeight, 0);
  checkCount("overscan", overscan, 0);
  if (typeof scrollTop !== "number" || !Number.isFinite(scrollTop)) {
    throw new ListError("Window", "scrollTop must be a finite number");
  }
  const total = count * rowHeight;
  const top = Math.min(Math.max(0, scrollTop), Math.max(0, total - viewportHeight));
  const firstVisible = Math.floor(top / rowHeight);
  const lastVisible = Math.ceil((top + viewportHeight) / rowHeight);
  const start = Math.max(0, firstVisible - overscan);
  const end = Math.min(count, Math.max(lastVisible, firstVisible + 1) + overscan);
  return { start, end, before: start * rowHeight, after: (count - end) * rowHeight };
}
