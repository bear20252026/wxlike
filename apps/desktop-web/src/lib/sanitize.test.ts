import { describe, expect, it } from "vitest";
import { sanitizePlainText, sanitizeRichText, sanitizeSvg } from "./sanitize";

describe("sanitizeRichText — XSS 向量", () => {
  it("script 标签被剥除", () => {
    const clean = sanitizeRichText('<script>alert("xss")</script>你好');
    expect(clean).not.toContain("<script");
    expect(clean).toContain("你好");
  });

  it("事件处理器属性被移除", () => {
    const clean = sanitizeRichText('<img src="https://ok/x.png" onerror="alert(1)">');
    expect(clean).not.toContain("onerror");
  });

  it("javascript: 链接被移除", () => {
    const clean = sanitizeRichText('<a href="javascript:alert(1)">点我</a>');
    expect(clean.toLowerCase()).not.toContain("javascript:");
  });

  it("data: URL 被移除", () => {
    const clean = sanitizeRichText('<a href="data:text/html,<script>alert(1)</script>">x</a>');
    expect(clean.toLowerCase()).not.toContain("data:text/html");
  });

  it("style 属性整体移除（CSS 注入路径）", () => {
    const clean = sanitizeRichText('<p style="background:url(javascript:alert(1))">t</p>');
    expect(clean).not.toContain("style");
    expect(clean).toContain("t");
  });

  it("富文本正文保留：链接与强调仍在", () => {
    const clean = sanitizeRichText('<a href="https://example.com/a?b=1">链接</a> 与 <strong>加粗</strong>');
    expect(clean).toContain('href="https://example.com/a?b=1"');
    expect(clean).toContain("<strong>加粗</strong>");
  });
});

describe("sanitizePlainText", () => {
  it("所有标签消失，纯文本保留", () => {
    expect(sanitizePlainText('<b>加粗</b><script>alert(1)</script>尾巴')).toBe("加粗尾巴");
  });
});

describe("sanitizeSvg — QR/图形路径", () => {
  it("svg 形状保留", () => {
    const clean = sanitizeSvg(
      '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 10 10"><path d="M0 0 L10 10" fill="#000"/></svg>',
    );
    expect(clean).toContain("<svg");
    expect(clean).toContain("<path");
  });

  it("svg 内嵌 script 与事件处理器被剥除", () => {
    const evil =
      '<svg xmlns="http://www.w3.org/2000/svg" onload="alert(1)"><script>alert(1)</script><circle r="1"/></svg>';
    const clean = sanitizeSvg(evil);
    expect(clean).not.toContain("onload");
    expect(clean).not.toContain("<script");
    expect(clean).toContain("circle");
  });
});
