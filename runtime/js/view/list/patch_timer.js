// Duration samples of the step "view diff and DOM patch" (ARCHITECTURE 5.8, budget 20 ms
// p99). The timer only records; official numbers come from the harness under bench/
// (MEASUREMENT 2 and R0.7), which reads the raw samples.

import { ListError } from "./sorted_index.js";

/** Target p99 of the view step in milliseconds (ARCHITECTURE 5.8). */
export const VIEW_PATCH_BUDGET_MS = 20;

function defaultNow() {
  return globalThis.performance.now();
}

/** Nearest rank percentile `p` (0 < p <= 100) of `samples`; NaN for no samples. */
export function percentile(samples, p) {
  if (!(p > 0 && p <= 100)) throw new ListError("Percentile", "p must be in (0, 100]");
  if (samples.length === 0) return Number.NaN;
  const sorted = Float64Array.from(samples).sort();
  return sorted[Math.ceil((p / 100) * sorted.length) - 1];
}

/**
 * Creates a timer. `now` returns milliseconds; `capacity` bounds the kept samples (the
 * oldest are dropped first and counted in `dropped`).
 */
export function createPatchTimer({ now = defaultNow, capacity = 100000 } = {}) {
  if (typeof now !== "function") throw new ListError("Timer", "now must be a function");
  if (!Number.isSafeInteger(capacity) || capacity < 1) {
    throw new ListError("Timer", "capacity must be a positive integer");
  }
  let samples = [];
  let dropped = 0;
  return {
    /** Runs `fn`, records its duration, returns its result. A throwing run is not recorded. */
    time(fn) {
      const start = now();
      const result = fn();
      samples.push(now() - start);
      if (samples.length > capacity) {
        samples.shift();
        dropped += 1;
      }
      return result;
    },
    /** Copy of the kept samples in milliseconds, oldest first. */
    samples() {
      return samples.slice();
    },
    get dropped() {
      return dropped;
    },
    p99() {
      return percentile(samples, 99);
    },
    /** Number of kept samples above `budget` milliseconds. */
    overBudget(budget = VIEW_PATCH_BUDGET_MS) {
      return samples.filter((ms) => ms > budget).length;
    },
    reset() {
      samples = [];
      dropped = 0;
    },
  };
}
