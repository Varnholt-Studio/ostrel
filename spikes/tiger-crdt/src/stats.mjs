// Nearest rank percentiles over raw samples (R0.2: raw samples are kept by the caller).
export function percentile(sorted, p) {
  if (sorted.length === 0) return NaN;
  const rank = Math.ceil((p / 100) * sorted.length);
  return sorted[Math.min(sorted.length, Math.max(1, rank)) - 1];
}

export function summary(samples) {
  const s = [...samples].sort((a, b) => a - b);
  const r = (x) => Math.round(x * 1000) / 1000;
  return {
    n: s.length,
    p50: r(percentile(s, 50)),
    p95: r(percentile(s, 95)),
    p99: r(percentile(s, 99)),
    max: r(s[s.length - 1]),
  };
}
