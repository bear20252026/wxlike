import { useCallback, useEffect, useRef, useState } from "react";
import type { DomainError } from "./core-contract";

export interface AsyncState<T> {
  data: T | null;
  error: DomainError | null;
  loading: boolean;
  reload: () => void;
  setData: (value: T | null) => void;
}

/** Fetch-on-mount with explicit loading/error and race protection. */
export function useAsync<T>(
  load: (signal: { cancelled: boolean }) => Promise<{ ok: true; data: T } | { ok: false; error: DomainError }>,
  deps: readonly unknown[],
): AsyncState<T> {
  const [data, setData] = useState<T | null>(null);
  const [error, setError] = useState<DomainError | null>(null);
  const [loading, setLoading] = useState(true);
  const [nonce, setNonce] = useState(0);
  const loaderRef = useRef(load);
  loaderRef.current = load;

  useEffect(() => {
    const signal = { cancelled: false };
    setLoading(true);
    setError(null);
    loaderRef
      .current(signal)
      .then((response) => {
        if (signal.cancelled) return;
        if (response.ok) {
          setData(response.data);
          setError(null);
        } else {
          setError(response.error);
        }
      })
      .catch((reason: unknown) => {
        if (signal.cancelled) return;
        setError({
          code: "INTERNAL",
          message: reason instanceof Error ? reason.message : String(reason),
          retryable: true,
          request_id: "ui",
        });
      })
      .finally(() => {
        if (!signal.cancelled) setLoading(false);
      });
    return () => {
      signal.cancelled = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [...deps, nonce]);

  const reload = useCallback(() => setNonce((value) => value + 1), []);
  return { data, error, loading, reload, setData };
}

export function useDebounced<T>(value: T, delayMs: number): T {
  const [debounced, setDebounced] = useState(value);
  useEffect(() => {
    const timer = setTimeout(() => setDebounced(value), delayMs);
    return () => clearTimeout(timer);
  }, [value, delayMs]);
  return debounced;
}

/** Idempotency guard for buttons: ignores clicks while a write is in flight. */
export function usePending() {
  const [pending, setPending] = useState(false);
  const ref = useRef(false);
  const run = useCallback(async <T,>(task: () => Promise<T>): Promise<T | null> => {
    if (ref.current) return null;
    ref.current = true;
    setPending(true);
    try {
      return await task();
    } finally {
      ref.current = false;
      setPending(false);
    }
  }, []);
  return { pending, run };
}
