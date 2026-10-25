// Typed guest SDK for the mini-host.v1 bridge.
//
// The guest never names its app, origin or a capability scope: the host
// transport attaches app/origin from the WebView session, and the broker
// maps each action to exactly one scope. There is no generic "invoke" here,
// so guest code cannot reach the main Tauri IPC through this module.

import {
  CONTRACT_VERSION,
  ERROR_CODES,
  METHODS,
  REQUEST_ID_PATTERN,
  SCOPES,
} from "./generated.js";

export { CONTRACT_VERSION, ERROR_CODES, METHODS, SCOPES };

export class MiniHostError extends Error {
  constructor(code, message, { method, requestId } = {}) {
    super(message || code);
    this.name = "MiniHostError";
    this.code = code;
    this.method = method;
    this.requestId = requestId;
  }
}

export function isMiniHostError(error) {
  return error instanceof MiniHostError;
}

function fallbackRequestId() {
  const bytes = new Uint8Array(16);
  globalThis.crypto.getRandomValues(bytes);
  return Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("");
}

function defaultRequestId() {
  return globalThis.crypto?.randomUUID?.() ?? fallbackRequestId();
}

/**
 * @param {(request: {action: string, request_id: string, params: object}) => Promise<object>} transport
 * @param {{requestId?: () => string}} [options]
 */
export function createClient(transport, options = {}) {
  if (typeof transport !== "function") {
    throw new TypeError("transport must be a function");
  }
  const nextRequestId = options.requestId ?? defaultRequestId;

  async function call(method, params) {
    const requestId = nextRequestId();
    if (typeof requestId !== "string" || !REQUEST_ID_PATTERN.test(requestId)) {
      throw new MiniHostError("invalid_call", "invalid request id", {
        method,
        requestId: String(requestId),
      });
    }
    let response;
    try {
      response = await transport({
        action: method,
        request_id: requestId,
        params,
      });
    } catch (cause) {
      throw new MiniHostError("host", cause?.message ?? "transport failed", {
        method,
        requestId,
      });
    }
    if (!response || typeof response !== "object" || typeof response.ok !== "boolean") {
      throw new MiniHostError("host", "malformed bridge response", {
        method,
        requestId,
      });
    }
    if (!response.ok) {
      const code = ERROR_CODES.includes(response.error?.code)
        ? response.error.code
        : "host";
      throw new MiniHostError(code, response.error?.message ?? code, {
        method,
        requestId,
      });
    }
    return response.result;
  }

  return Object.freeze({
    contractVersion: CONTRACT_VERSION,
    storage: Object.freeze({
      get: (key) => call(METHODS.storageGet, { key }),
      set: (key, value) => call(METHODS.storageSet, { key, value }),
      remove: (key) => call(METHODS.storageRemove, { key }),
    }),
    network: Object.freeze({
      request: (url, { method = "GET", body } = {}) =>
        call(
          METHODS.networkRequest,
          body === undefined ? { url, method } : { url, method, body },
        ),
    }),
    camera: Object.freeze({
      capture: () => call(METHODS.cameraCapture, {}),
    }),
    location: Object.freeze({
      get: () => call(METHODS.locationGet, {}),
    }),
    file: Object.freeze({
      pick: (purpose) => call(METHODS.filePick, { purpose }),
    }),
    payments: Object.freeze({
      pay: ({ amount, currency, orderId }) =>
        call(METHODS.payRequest, {
          amount,
          currency,
          order_id: orderId,
        }),
    }),
  });
}
