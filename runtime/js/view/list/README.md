# runtime/js/view/list

Long lists for views (work package T3-1c, ARCHITECTURE 5.8): an ordered index with indexed keys, virtualisation over the view core, and duration samples of the view patch step.

| File | Content |
|---|---|
| `sorted_index.js` | `SortedIndex`: row ids ordered by (sort key, id), batch updates, `indexOf` in O(log n); `compareSortKeys`; typed `ListError` |
| `window.js` | `computeWindow`: rows to render for a scroll position, plus spacer heights |
| `virtual_list.js` | `createVirtualList(renderer, sizer, options)` with `mount`, `update`, `unmount`; `createStyleSizer` for browsers |
| `patch_timer.js` | `createPatchTimer`, `percentile`, `VIEW_PATCH_BUDGET_MS` (20) |
| `list.test.mjs` | Unit tests, run by `ci/checks/50_tests.sh` with `node --test` |

## Rules

* The index searches only with the key each id was indexed under, never with current row values. A batch first takes out every id that leaves or changes its key, then inserts under the new keys, so several rows can move in one batch without losing track (TIGER-1 finding 1).
* A batch is validated completely before the first change. Ids are strings; sort keys are strings, finite numbers or flat arrays of those. Numbers sort before strings, strings before arrays; strings compare by code point (D50, D61); ties break on the id.
* Only rows in the window (viewport plus `overscan` rows each side) have DOM nodes. Rows have one fixed height (`rowHeight`, whole pixels).
* Row content is rendered once while a row stays in the window and again only when its id is passed in `changed`. Callers pass every id whose content changed in the batch; ids outside the window cost nothing.
* Structure: an outer element with class `ostrel-vlist`, a top spacer, the element with role `list` and one wrapper with role `listitem`, `aria-posinset` and `aria-setsize` per rendered row, and a bottom spacer. Spacers are `aria-hidden`.
* Spacer heights are written by the sizer, which sets only the CSS property `height` through the CSSOM and only to a whole number of pixels from 0 to 2^31 - 1. The style attribute itself stays off the allowlist (`safe/attrs.js`).
* A failed update (invalid input or a throwing `renderRow`) leaves the mounted DOM as it was.

## Measurement

`createPatchTimer` records one sample per `update` when passed as `timer`. Samples are raw milliseconds from the given clock (default `performance.now`). The official p99 of the view step is measured by the harness under `bench/` (MEASUREMENT 2, R0.7). The unit test checks the CPU time of the patch on 10 000 rows in the fake DOM; that value shows the work stays bounded, it is not a KPI value.

## Error codes

`IdType`, `KeyType`, `DuplicateUpdate`, `IndexDesync`, `Window`, `Options`, `RowType`, `Mounted`, `BlockSize`, `Percentile`, `Timer`.
