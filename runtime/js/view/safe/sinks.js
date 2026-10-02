// Safe attribute sinks for the view core (ARCHITECTURE 7.5). The renderer in
// runtime/js/view/core.js writes attributes only through an object with
// setAttr(element, name, value) and removeAttr(element, name); safeSinks is the
// production implementation of that object.
//
// Behaviour:
//   * A name that is not on the allowlist (attrs.js) for the element's tag is a
//     SinkError. The compiler emits attribute names, so this is a bug, never user data.
//   * A value outside its rule (wrong enum word, non integer, bad flag) is a SinkError.
//   * A URL that parseUrl refuses removes the attribute instead of throwing. The value
//     comes from user data, and the view must keep working: a link without href is
//     inert text, an image without src shows nothing (AC-47).

import { ruleFor, valueAllowed } from "./attrs.js";
import { parseUrl } from "./url.js";

export class SinkError extends Error {
  constructor(code, message) {
    super(message);
    this.name = "SinkError";
    this.code = code;
  }
}

// The lowercase tag name of an HTML element (localName), with tagName as a fallback for
// minimal DOM stand ins.
function tagOf(element) {
  const tag = element.localName ?? element.tagName;
  if (typeof tag !== "string") throw new SinkError("Element", "sink target is not an element");
  return tag.toLowerCase();
}

function checkedRule(element, name) {
  const tag = tagOf(element);
  const rule = typeof name === "string" ? ruleFor(tag, name) : undefined;
  if (rule === undefined) {
    throw new SinkError("AttrName", `attribute ${JSON.stringify(String(name))} is not allowed on <${tag}>`);
  }
  return rule;
}

export function setAttr(element, name, value) {
  const rule = checkedRule(element, name);
  if (typeof value !== "string") {
    throw new SinkError("AttrType", `attribute ${name} must be a string`);
  }
  if (rule === "url") {
    const url = parseUrl(value);
    if (url === null) element.removeAttribute(name);
    else element.setAttribute(name, url);
    return;
  }
  if (!valueAllowed(rule, name, value)) {
    throw new SinkError("AttrValue", `value ${JSON.stringify(value)} is not allowed for ${name}`);
  }
  element.setAttribute(name, value);
}

export function removeAttr(element, name) {
  checkedRule(element, name);
  element.removeAttribute(name);
}

export const safeSinks = Object.freeze({ setAttr, removeAttr });
