// In-process limits. Values follow the Ostrel limits table (ARCHITECTURE 5.9) so both
// stacks are measured under the same rules. State is per process and lost on restart.

export const LIMITS = {
  bodyBytes: 1024 * 1024,
  frameBytes: 1024 * 1024,
  messageChars: 2000,
  opsPerSecond: 50,
  opsBurst: 200,
  rowsPerHour: 1000,
  challengesPerMinute: 10,
  registrationsPerHour: 5,
  challengeTtlMs: 60_000,
  clockToleranceMs: 2000,
};

// Token bucket: `rate` tokens per second, at most `burst` stored.
export class Bucket {
  rate: number;
  burst: number;
  tokens: number;
  at: number;

  constructor(rate: number, burst: number, now = Date.now()) {
    this.rate = rate;
    this.burst = burst;
    this.tokens = burst;
    this.at = now;
  }

  available(now = Date.now()): number {
    this.tokens = Math.min(this.burst, this.tokens + ((now - this.at) / 1000) * this.rate);
    this.at = now;
    return this.tokens;
  }

  take(n: number, now = Date.now()): boolean {
    if (n > this.available(now)) return false;
    this.tokens -= n;
    return true;
  }
}

// Fixed count per sliding window, keyed (by IP, user, ...).
export class Window {
  max: number;
  ms: number;
  hits = new Map<string, number[]>();

  constructor(max: number, ms: number) {
    this.max = max;
    this.ms = ms;
  }

  remaining(key: string, now = Date.now()): number {
    const recent = (this.hits.get(key) ?? []).filter((t) => t > now - this.ms);
    this.hits.set(key, recent);
    return this.max - recent.length;
  }

  allow(key: string, n = 1, now = Date.now()): boolean {
    if (n > this.remaining(key, now)) return false;
    const recent = this.hits.get(key) ?? [];
    for (let i = 0; i < n; i++) recent.push(now);
    this.hits.set(key, recent);
    return true;
  }
}
