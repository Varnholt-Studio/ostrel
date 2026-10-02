// Attribute allowlist for views (ARCHITECTURE 7.5). An attribute reaches the DOM only when
// its name is listed here, either for every element or for the element's tag. Each entry
// names the kind of value it accepts:
//   "text"   any string; the DOM treats it as plain data
//   "int"    a decimal integer, optionally negative
//   "flag"   a boolean attribute; the value must be "" (present) or the attribute's name
//   "url"    checked by parseUrl (url.js); a refused URL leaves the attribute unset
//   a Set    one of the listed words, compared exactly
//
// Not listed on purpose: event handlers (on*), style (CSS can load URLs; styling goes
// through classes and the std theme), id and name (DOM clobbering, and acceptance tests
// use roles and visible text, A2-10), form actions, frame and embed attributes, and
// namespaced names such as xlink:href.

const DIR = new Set(["ltr", "rtl", "auto"]);
const BUTTON_TYPES = new Set(["button", "submit", "reset"]);
const INPUT_TYPES = new Set([
  "checkbox",
  "date",
  "email",
  "number",
  "password",
  "radio",
  "search",
  "tel",
  "text",
  "time",
  "url",
]);
const ARIA_CURRENT = new Set(["page", "step", "location", "date", "time", "true", "false"]);
const AUTOCOMPLETE = new Set(["on", "off", "username", "current-password", "new-password", "email"]);
const LOADING = new Set(["lazy", "eager"]);

// Attributes allowed on every element.
const GLOBAL = new Map([
  ["class", "text"],
  ["title", "text"],
  ["lang", "text"],
  ["dir", DIR],
  ["role", "text"],
  ["tabindex", "int"],
  ["hidden", "flag"],
  ["aria-current", ARIA_CURRENT],
  // State markers the std theme styles (std/theme, T3-3).
  ["data-look", "text"],
  ["data-pending", "text"],
  ["data-rejected", "text"],
]);

// Any aria-* attribute other than aria-current carries text only.
const ARIA_NAME = /^aria-[a-z]+$/;

// Attributes allowed only on the named tag.
const BY_TAG = new Map([
  ["a", new Map([["href", "url"]])],
  [
    "img",
    new Map([
      ["src", "url"],
      ["alt", "text"],
      ["width", "int"],
      ["height", "int"],
      ["loading", LOADING],
    ]),
  ],
  [
    "button",
    new Map([
      ["type", BUTTON_TYPES],
      ["disabled", "flag"],
    ]),
  ],
  [
    "input",
    new Map([
      ["type", INPUT_TYPES],
      ["value", "text"],
      ["placeholder", "text"],
      ["autocomplete", AUTOCOMPLETE],
      ["maxlength", "int"],
      ["minlength", "int"],
      ["min", "text"],
      ["max", "text"],
      ["step", "text"],
      ["disabled", "flag"],
      ["readonly", "flag"],
      ["required", "flag"],
      ["checked", "flag"],
    ]),
  ],
  [
    "textarea",
    new Map([
      ["placeholder", "text"],
      ["rows", "int"],
      ["maxlength", "int"],
      ["disabled", "flag"],
      ["readonly", "flag"],
      ["required", "flag"],
    ]),
  ],
  [
    "select",
    new Map([
      ["disabled", "flag"],
      ["required", "flag"],
    ]),
  ],
  [
    "option",
    new Map([
      ["value", "text"],
      ["selected", "flag"],
      ["disabled", "flag"],
    ]),
  ],
]);

const INTEGER = /^-?[0-9]{1,9}$/;

/** Returns the value rule for an attribute on a tag, or undefined when it is not allowed. */
export function ruleFor(tag, name) {
  const own = BY_TAG.get(tag);
  if (own !== undefined && own.has(name)) return own.get(name);
  if (GLOBAL.has(name)) return GLOBAL.get(name);
  if (ARIA_NAME.test(name)) return "text";
  return undefined;
}

/**
 * Checks a value against its rule. Returns true when it is allowed. URL rules are not
 * decided here, because a refused URL is not an error (see sinks.js).
 */
export function valueAllowed(rule, name, value) {
  if (rule instanceof Set) return rule.has(value);
  switch (rule) {
    case "text":
      return true;
    case "int":
      return INTEGER.test(value);
    case "flag":
      return value === "" || value === name;
    default:
      return false;
  }
}
