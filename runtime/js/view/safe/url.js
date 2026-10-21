// URL rule for views (ARCHITECTURE 7.5, SPEC 12.13, SYNTAX 4.8): a URL may be relative, or
// absolute with the scheme http, https or mailto. Everything else (javascript:, data:,
// vbscript:, blob:, file: and any unknown scheme) is refused, so the caller renders it as
// inert text.
//
// The check follows how browsers read a URL attribute, so a value that looks harmless here
// cannot turn into a script URL in the DOM:
//   * browsers strip leading and trailing spaces and C0 controls, and drop tab, CR and LF
//     anywhere ("java\tscript:" is javascript:). We refuse every control character instead
//     of reproducing that cleanup, and trim only plain spaces.
//   * the scheme is the run of [A-Za-z][A-Za-z0-9+.-]* before the first ":", compared
//     without case. A ":" after "/", "?" or "#" belongs to a relative URL.
//
// An http or https URL must also pass the host rule H (D144, ARCHITECTURE 7.5). The server
// applies the same rule in Url.parse, and both sides are tested against one corpus,
// tests/hostile/url/corpus.txt. The rule is deliberately narrower than the WHATWG URL
// Standard: host names only in ASCII (international names in their xn-- form), no IPv6
// literals, no user info, no empty port. It decides only whether a link is usable;
// security rests on the control character rule and the scheme allowlist above.

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
  if (scheme === "mailto") return url;

  const afterScheme = url.slice(match[0].length);
  return passesHostRule(afterScheme) ? url : null;
}

// Host rule H for the text after "http:" or "https:".
function passesHostRule(afterScheme) {
  // H1: the scheme is followed by "//".
  if (!afterScheme.startsWith("//")) return false;

  // H2: the authority runs up to the first "/", "?", "#" or the end.
  // H7: what follows (path, query, fragment) is not checked.
  const authority = authorityOf(afterScheme.slice(2));

  // H3: the authority is a host, optionally followed by ":" and a port. Any other
  // character ("@", "[", "]", "\", "%", space, non ASCII) fails the host or port check.
  const colon = authority.indexOf(":");
  const host = colon === -1 ? authority : authority.slice(0, colon);
  if (colon !== -1 && !isPort(authority.slice(colon + 1))) return false;

  return isHost(host);
}

function authorityOf(text) {
  const end = text.search(/[/?#]/);
  return end === -1 ? text : text.slice(0, end);
}

// H4 and H5: one or more labels of [A-Za-z0-9-] separated by single dots, at most one
// trailing dot. A host that looks numeric must be a plain IPv4 address.
function isHost(host) {
  const withoutTrailingDot = host.endsWith(".") ? host.slice(0, -1) : host;
  const labels = withoutTrailingDot.split(".");
  if (!labels.every(isLabel)) return false; // also refuses an empty host

  const last = labels[labels.length - 1];
  if (looksNumeric(last)) return isIpv4(labels);
  return true;
}

const LABEL = /^[A-Za-z0-9-]+$/;

function isLabel(label) {
  return LABEL.test(label);
}

function looksNumeric(label) {
  return /^[0-9]+$/.test(label) || /^0x/i.test(label);
}

// Exactly four decimal parts from 0 to 255, without leading zeros.
function isIpv4(labels) {
  return labels.length === 4 && labels.every(isIpv4Part);
}

function isIpv4Part(label) {
  return /^(0|[1-9][0-9]{0,2})$/.test(label) && Number(label) <= 255;
}

// H6: 1 to 5 digits with a value of at most 65535.
function isPort(port) {
  return /^[0-9]{1,5}$/.test(port) && Number(port) <= 65535;
}
