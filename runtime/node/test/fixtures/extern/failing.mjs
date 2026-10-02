// Extern module used by the sidecar failure tests. Every export misbehaves in one way an app's
// `extern js server` module could, except `echo`, which proves the host still works afterwards.
import { readFileSync } from 'node:fs';

export function echo(value) {
  return value;
}

export function throwsError() {
  throw new Error('extern failure');
}

export function throwsNonError() {
  throw 'a plain string';
}

export async function rejects() {
  throw new Error('async extern failure');
}

export function neverSettles() {
  return new Promise(() => {});
}

export function busyLoop() {
  for (;;) {
    // Blocks the event loop: only the supervisor's timeout can end this call.
  }
}

export function exitsProcess() {
  process.exit(3);
}

export function uncaughtLater() {
  setTimeout(() => {
    throw new Error('uncaught after return');
  }, 0);
  return 'returned';
}

export function logsToStdout() {
  console.log('stray output from an extern module');
  console.info('stray info output');
  return 'logged';
}

export function hugeResult() {
  return 'x'.repeat(2 * 1024 * 1024);
}

export function nanResult() {
  return Number.NaN;
}

export function infinityResult() {
  return Number.POSITIVE_INFINITY;
}

export function bigintResult() {
  return 10n;
}

export function cyclicResult() {
  const a = {};
  a.self = a;
  return a;
}

export function functionResult() {
  return () => 1;
}

export function readsOutsideExternDir() {
  return readFileSync('/etc/hostname', 'utf8');
}

export function envKeys() {
  return Object.keys(process.env);
}
