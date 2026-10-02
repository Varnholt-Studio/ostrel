// Build manifest of the offline app shell (ARCHITECTURE 5.10).
//
// The JS backend writes one manifest per build. It lists every file of the app shell
// (HTML entry, JS modules, CSS, std theme, icons) relative to the service worker scope.
// The service worker caches exactly these files and nothing else, so Cache Storage can
// never hold an API response, row data or a token (SPEC AC-62).

/** Prefix of every cache owned by the app shell. Other caches of the origin are left alone. */
export const CACHE_PREFIX = 'ostrel-shell-';

/** Upper bound on files per build, so a corrupt manifest cannot start an unbounded install. */
export const MAX_FILES = 4096;

const BUILD_RE = /^[0-9a-f]{16,64}$/;
// A shell path is a relative URL path: segments of safe characters joined by '/'.
const SEGMENT_RE = /^[A-Za-z0-9._~@+-]+$/;

/** Thrown for any manifest that does not satisfy the rules below. */
export class ManifestError extends Error {
  constructor(message) {
    super(message);
    this.name = 'ManifestError';
  }
}

function checkPath(path, what) {
  if (typeof path !== 'string' || path.length === 0 || path.length > 512) {
    throw new ManifestError(`${what}: expected a non empty path of at most 512 characters`);
  }
  for (const segment of path.split('/')) {
    if (!SEGMENT_RE.test(segment) || segment === '.' || segment === '..') {
      throw new ManifestError(`${what}: invalid path ${JSON.stringify(path)}`);
    }
  }
}

/**
 * Validates a manifest and returns a frozen, normalised copy.
 *
 * Shape: { build: hex string (16 to 64 digits), entry: path, files: [path, ...],
 * routes?: [path, ...] }. `entry` must be one of `files`. `routes` are extra navigation
 * paths that are answered with the entry document; the scope root always is one.
 *
 * @param {unknown} raw
 * @returns {{build: string, entry: string, files: readonly string[], routes: readonly string[], cacheName: string}}
 */
export function parseManifest(raw) {
  if (raw === null || typeof raw !== 'object' || Array.isArray(raw)) {
    throw new ManifestError('manifest: expected an object');
  }
  const { build, entry, files, routes = [] } = raw;
  if (typeof build !== 'string' || !BUILD_RE.test(build)) {
    throw new ManifestError('build: expected 16 to 64 lowercase hex digits');
  }
  if (!Array.isArray(files) || files.length === 0 || files.length > MAX_FILES) {
    throw new ManifestError(`files: expected 1 to ${MAX_FILES} paths`);
  }
  if (!Array.isArray(routes) || routes.length > MAX_FILES) {
    throw new ManifestError(`routes: expected at most ${MAX_FILES} paths`);
  }
  const seen = new Set();
  for (const file of files) {
    checkPath(file, 'files');
    if (seen.has(file)) throw new ManifestError(`files: duplicate path ${JSON.stringify(file)}`);
    seen.add(file);
  }
  checkPath(entry, 'entry');
  if (!seen.has(entry)) throw new ManifestError('entry: must be listed in files');
  for (const route of routes) checkPath(route, 'routes');
  return Object.freeze({
    build,
    entry,
    files: Object.freeze([...files]),
    routes: Object.freeze([...new Set(routes)]),
    cacheName: cacheNameFor(build),
  });
}

/** Name of the cache that holds the shell of one build. */
export function cacheNameFor(build) {
  return CACHE_PREFIX + build;
}

/** True for caches created by the app shell (of any build). */
export function isShellCache(name) {
  return typeof name === 'string' && name.startsWith(CACHE_PREFIX);
}
