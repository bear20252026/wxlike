import "@testing-library/jest-dom/vitest";

// jsdom has no clipboard; the PlatformPort web implementation falls back to a
// textarea copy, but tests that assert on clipboard content install this.
if (typeof navigator !== "undefined" && !navigator.clipboard) {
  Object.defineProperty(navigator, "clipboard", {
    configurable: true,
    value: {
      writeText: async (text: string) => {
        (globalThis as Record<string, unknown>).__lastClipboardWrite = text;
      },
      readText: async () =>
        String((globalThis as Record<string, unknown>).__lastClipboardWrite ?? ""),
    },
  });
}

if (typeof navigator !== "undefined" && !("mediaDevices" in navigator)) {
  Object.defineProperty(navigator, "mediaDevices", {
    configurable: true,
    value: {
      enumerateDevices: async () => [],
      getUserMedia: async () => {
        throw new DOMException("Not allowed", "NotAllowedError");
      },
    },
  });
}
