/**
 * Rich text gate (G11-01 requirement 2): 富文本只渲染净化结构.
 *
 * Everything user- or server-supplied that reaches `dangerouslySetInnerHTML`
 * goes through `sanitizeRichText`. The allow-list is deliberately tighter than
 * DOMPurify defaults: no forms, no media elements, no iframes, no event
 * handlers, no `style` (CSS injection is a phishing vector, not just
 * cosmetics). URLs are restricted to http(s)/mailto; `javascript:` and
 * `data:` die here even though DOMPurify would also catch them — defense in
 * depth is the point of the second layer.
 */

import DOMPurify from "dompurify";

const FORBIDDEN_TAGS = ["style", "form", "input", "button", "textarea", "select", "iframe", "object", "embed", "video", "audio", "script", "svg", "math"];

const purifier = DOMPurify(window);

purifier.addHook("uponSanitizeAttribute", (_node, data) => {
  // style="…" is removed wholesale; look for the attribute by its config name
  if (data.attrName === "style") data.keepAttr = false;
});

const CONFIG = {
  USE_PROFILES: { html: true },
  FORBID_TAGS: FORBIDDEN_TAGS,
  FORBID_ATTR: ["style", "srcset"],
  ALLOWED_URI_REGEXP: /^(?:(?:https?|mailto):|[^a-z]|[a-z+.-]+(?:[^a-z+.\-:]|$))/i,
  KEEP_CONTENT: true,
  RETURN_DOM_FRAGMENT: false,
} satisfies Parameters<typeof purifier.sanitize>[1];

/** Returns markup that is safe to hand to `dangerouslySetInnerHTML`. */
export function sanitizeRichText(raw: string): string {
  const clean = purifier.sanitize(raw ?? "", CONFIG);
  // Without RETURN_TRUSTED_TYPE the value is a plain string; TrustedHTML's
  // toString() is the same markup, so the cast keeps both shapes honest.
  return typeof clean === "string" ? clean : String(clean);
}

/**
 * SVG profile: allows the svg vocabulary (shapes, paths, rect/circle…) for
 * locally generated artwork (QR codes), still strips scripts, foreignObject,
 * event handlers and style. URIs stay on the http(s)/mailto allow-list.
 */
const SVG_CONFIG = {
  USE_PROFILES: { svg: true, svgFilters: true },
  FORBID_TAGS: ["script", "foreignObject", "iframe", "object", "embed"],
  FORBID_ATTR: ["style", "srcset"],
  ALLOWED_URI_REGEXP: /^(?:(?:https?|mailto):|[^a-z]|[a-z+.-]+(?:[^a-z+.\-:]|$))/i,
  KEEP_CONTENT: false,
  RETURN_DOM_FRAGMENT: false,
  ADD_TAGS: ["svg"],
  ADD_ATTR: ["viewBox", "xmlns", "xmlns:xlink"],
} satisfies Parameters<typeof purifier.sanitize>[1];

/** Returns SVG markup safe for `dangerouslySetInnerHTML` (scripts stripped). */
export function sanitizeSvg(raw: string): string {
  const clean = purifier.sanitize(raw ?? "", SVG_CONFIG);
  return typeof clean === "string" ? clean : String(clean);
}

/** Text-only projection: tags dropped, entities decoded, no markup survives. */
export function sanitizePlainText(raw: string): string {
  const div = document.createElement("div");
  div.innerHTML = purifier.sanitize(raw ?? "", { ...CONFIG, USE_PROFILES: { html: false } });
  return div.textContent ?? "";
}
