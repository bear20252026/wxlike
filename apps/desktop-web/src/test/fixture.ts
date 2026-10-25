/**
 * Test transport helpers (FP-G11).
 *
 * `fixtureTransport` builds a `Transport` from a route table so feature suites
 * exercise the real domain client code against canned server responses. This
 * is the sanctioned parallel-development mode (上游卡未合入时按冻结 fixture
 * 开发) — final integration acceptance still runs these same suites against
 * the real server via `setTransport(fetchTransport)`.
 */

import { ApiError, type Transport } from "../lib/http";

export type RouteTable = Record<string, unknown> & {
  [route: string]: unknown;
};

export type FixtureHandler = (
  method: string,
  path: string,
  body: unknown,
  opts: { idempotencyKey?: string; signal?: AbortSignal },
) => unknown;

/** Match "METHOD /path" with `:param` placeholders. */
export function matchRoute(pattern: string, method: string, path: string): Record<string, string> | null {
  const [patternMethod, patternPath] = pattern.split(" ", 2);
  if (!patternMethod || !patternPath) return null;
  if (patternMethod !== "*" && patternMethod !== method) return null;
  const patternParts = patternPath.split("/").filter(Boolean);
  const pathParts = path.split("?")[0]?.split("/").filter(Boolean) ?? [];
  if (patternParts.length !== pathParts.length) return null;
  const params: Record<string, string> = {};
  for (let i = 0; i < patternParts.length; i++) {
    const expected = patternParts[i];
    const actual = pathParts[i];
    if (expected === undefined || actual === undefined) return null;
    if (expected.startsWith(":")) {
      params[expected.slice(1)] = decodeURIComponent(actual);
    } else if (expected !== actual) {
      return null;
    }
  }
  return params;
}

export function fixtureTransport(routes: Record<string, FixtureHandler>): Transport {
  return async (method, path, opts = {}) => {
    for (const [pattern, handler] of Object.entries(routes)) {
      const params = matchRoute(pattern, method, path);
      if (!params) continue;
      const result = handler(method, path, opts.body, {
        idempotencyKey: opts.idempotencyKey,
        signal: opts.signal,
      });
      // `() => err(403, …)` (returned) and `() => { throw err(403, …) }` (thrown)
      // both mean "the server answered with this error".
      if (result instanceof ApiError) throw result;
      return await raceAbort(result, opts.signal);
    }
    throw new ApiError(404, `fixture 未实现: ${method} ${path}`, path, "not_found");
  };
}

/** Fixture handlers ignore cancellation; mirror the real transport's abort contract. */
function raceAbort<T>(result: T | Promise<T>, signal: AbortSignal | undefined): Promise<T> {
  const value = Promise.resolve(result);
  if (!signal) return value;
  return new Promise<T>((resolve, reject) => {
    const onAbort = () =>
      reject(new ApiError(0, "已取消", "fixture", "cancelled"));
    if (signal.aborted) {
      onAbort();
      return;
    }
    signal.addEventListener("abort", onAbort, { once: true });
    value.then(
      (v) => {
        signal.removeEventListener("abort", onAbort);
        resolve(v);
      },
      (e: unknown) => {
        signal.removeEventListener("abort", onAbort);
        reject(e instanceof Error ? e : new Error(String(e)));
      },
    );
  });
}

export function err(status: number, message: string, code?: ApiError["code"]): ApiError {
  return new ApiError(status, message, "fixture", code);
}

/** Stateful in-memory store for duplicate-submit / idempotency assertions. */
export class CallLog {
  private calls: Array<{ method: string; path: string; idempotencyKey?: string; body?: unknown }> = [];

  record(method: string, path: string, idempotencyKey?: string, body?: unknown): void {
    this.calls.push({ method, path, idempotencyKey, body });
  }

  count(match: { method?: string; path?: string; idempotencyKeyPrefix?: string }): number {
    return this.all(match).length;
  }

  all(match: { method?: string; path?: string; idempotencyKeyPrefix?: string }): Array<{ method: string; path: string; idempotencyKey?: string; body?: unknown }> {
    return this.calls.filter((call) => {
      if (match.method && call.method !== match.method) return false;
      if (match.path && call.path !== match.path) return false;
      if (
        match.idempotencyKeyPrefix &&
        !(call.idempotencyKey ?? "").startsWith(match.idempotencyKeyPrefix)
      )
        return false;
      return true;
    });
  }
}

/** A transport that logs calls then delegates to a route table. */
export function loggingTransport(routes: Record<string, FixtureHandler>, log: CallLog): Transport {
  const inner = fixtureTransport(routes);
  return async (method, path, opts = {}) => {
    log.record(method, path, opts.idempotencyKey, opts.body);
    return inner(method, path, opts);
  };
}
