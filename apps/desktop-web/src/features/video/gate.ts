/**
 * Playback visibility gate (G11-03 requirement 1): 播放器订阅可见页面，
 * 不可见时暂停释放。Two inputs — document visibility and viewport presence —
 * both degrade gracefully where the API is missing (tests, old webviews).
 */

import { useEffect, useRef, useState } from "react";

export function usePlaybackGate(ref: React.RefObject<HTMLElement | null>): boolean {
  const [documentVisible, setDocumentVisible] = useState(
    () => typeof document === "undefined" || document.visibilityState !== "hidden",
  );
  const [inViewport, setInViewport] = useState(true);
  const observerRef = useRef<IntersectionObserver | null>(null);

  useEffect(() => {
    const onChange = () => setDocumentVisible(document.visibilityState !== "hidden");
    document.addEventListener("visibilitychange", onChange);
    return () => document.removeEventListener("visibilitychange", onChange);
  }, []);

  useEffect(() => {
    const element = ref.current;
    if (!element || typeof IntersectionObserver === "undefined") return;
    const observer = new IntersectionObserver(
      (entries) => {
        setInViewport(entries.some((entry) => entry.isIntersecting));
      },
      { threshold: 0.25 },
    );
    observer.observe(element);
    observerRef.current = observer;
    return () => {
      observer.disconnect();
      observerRef.current = null;
    };
  }, [ref]);

  return documentVisible && inViewport;
}
