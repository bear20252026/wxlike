import { domainError, fail, type CoreCapabilities, type CoreHandle, type EventCursor } from "../core-contract";

/**
 * Production web builds have no Tauri host and the WASM core (G01-03) is not
 * merged yet. Every action fails explicitly instead of falling back to demo data.
 */
export function createUnavailableCore(reason: string): CoreHandle {
  return {
    async dispatch(request) {
      return fail(domainError("DEPENDENCY_UNAVAILABLE", reason, request.request_id, { retryable: false }));
    },
    subscribe(_cursor: EventCursor | null) {
      return () => undefined;
    },
    async capabilities(): Promise<CoreCapabilities> {
      return {
        contract_version: null,
        transport: "wasm",
        actions: [],
        qr_login: false,
        secrets_os_backed: false,
        sync_range: false,
        notes: [reason],
      };
    },
  };
}
