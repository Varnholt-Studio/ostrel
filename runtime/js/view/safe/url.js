// URL rule for views (ARCHITECTURE 7.5, SYNTAX 4.8): a URL may be relative, or absolute
// with the scheme http, https or mailto. Everything else (javascript:, data:, vbscript:,
// blob:, file: and any unknown scheme) is refused, so the caller renders it as inert text.
//
// The check follows how browsers read a URL attribute, so a value that looks harmless here
// cannot turn into a script URL in the DOM:
//   * browsers strip leading and trailing spaces and C0 controls, and drop tab, CR and LF
//     anywhere ("java\tscript:" is javascript:). We refuse every control character instead
//     of reproducing that cleanup, and trim only plain spaces.
//   * the scheme is the run of [A-Za-z][A-Za-z0-9+.-]* before the first ":", compared
//     without case. A ":" after "/", "?" or "#" belongs to a relative URL.

const ALLOWED_SCHEMES = new Set(["http", "https", "mailto"]);

const SCHEME = /^([A-Za-z][A-Za-z0-9+.-]*):/;

// U+0000 to U+001F and U+007F to U+009F (C0 and C1 controls, DEL).
const CONTROL = /[\u0000-\u001f\u007f-\u009f]/;

/**
 * Returns the URL text to put into the attribute, or null when the value is not an
 * allowed URL. The returned text is the input without surrounding spaces; it is not
 * normalised further, so what the developer wrote is what the DOM gets.
 */
export function parseUrl(value) {
  if (typeof value !== "string") return null;
  if (CONTROL.test(value)) return null;
  const url = value.replace(/^ +| +$/g, "");
  if (url === "") return null;

  const match = SCHEME.exec(url);
  if (match === null) return url; // relative: path, query, fragment or "//host/path"

  const scheme = match[1].toLowerCase();
  if (!ALLOWED_SCHEMES.has(scheme)) return null;
  if (scheme !== "mailto" && !isParsableUrl(url)) return null;
  return url;
}

function isParsableUrl(url) {
  try {
    new URL(url);
    return true;
  } catch {
    return false;
  }
}
