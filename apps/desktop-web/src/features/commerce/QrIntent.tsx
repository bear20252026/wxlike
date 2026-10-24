/**
 * QR intent display (G11-04 requirement 2): the receiving QR comes from a
 * short-lived server intent — never a random string, never a sample amount.
 * The component re-checks `expires_at` on a timer: once expired the code is
 * visibly dead (截图二维码过期失效) and a fresh intent must be requested.
 *
 * Renders the QR as inline SVG (scannable, no canvas dependency) via the
 * `qrcode` package.
 */

import { useEffect, useState } from "react";
import QRCode from "qrcode";
import { formatMoney } from "../../lib/money";
import type { PaymentIntent } from "./model";

export function QrIntentPanel({
  intent,
  onExpired,
  onRefresh,
  refreshing,
}: {
  intent: PaymentIntent | null;
  onExpired?: () => void;
  onRefresh?: () => void;
  refreshing?: boolean;
}) {
  const [svg, setSvg] = useState("");
  const [expired, setExpired] = useState(false);

  useEffect(() => {
    let alive = true;
    setSvg("");
    if (!intent) return;
    void QRCode.toString(intent.qr_payload, { type: "svg", margin: 1, width: 220 })
      .then((markup) => {
        if (alive) setSvg(markup);
      })
      .catch(() => {
        if (alive) setSvg("");
      });
    return () => {
      alive = false;
    };
  }, [intent]);

  useEffect(() => {
    if (!intent) return;
    const check = () => {
      if (Date.now() >= intent.expires_at) {
        setExpired(true);
        onExpired?.();
      }
    };
    check();
    const timer = setInterval(check, 1_000);
    return () => clearInterval(timer);
  }, [intent, onExpired]);

  if (!intent) {
    return (
      <div className="flex aspect-square items-center justify-center rounded-xl border border-dashed border-zinc-300 text-sm text-zinc-400" data-testid="qr-empty">
        生成收款码…
      </div>
    );
  }

  return (
    <div className="flex flex-col items-center gap-2 rounded-xl border border-zinc-200 bg-white p-4" data-testid="qr-intent">
      {expired ? (
        <div className="relative flex aspect-square w-56 items-center justify-center" data-testid="qr-expired">
          <div className="absolute inset-0 rounded-lg bg-white/85" />
          <div className="relative z-10 text-center">
            <p className="text-base font-semibold text-red-600">二维码已过期</p>
            <p className="mt-1 text-xs text-zinc-500">截图的二维码无法继续使用</p>
            {onRefresh ? (
              <button
                type="button"
                onClick={onRefresh}
                disabled={refreshing}
                className="mt-2 rounded-lg bg-emerald-600 px-3 py-1.5 text-sm text-white hover:bg-emerald-700 disabled:opacity-50"
              >
                {refreshing ? "刷新中…" : "重新生成"}
              </button>
            ) : null}
          </div>
        </div>
      ) : svg ? (
        <div className="w-56 [&>svg]:h-full [&>svg]:w-full" data-testid="qr-svg" dangerouslySetInnerHTML={{ __html: svg }} />
      ) : (
        <div className="aspect-square w-56 animate-pulse rounded-lg bg-zinc-100" />
      )}
      <p className="text-sm text-zinc-700" data-testid="qr-amount">
        {formatMoney(intent.amount_minor)}
      </p>
      <p className="text-xs text-zinc-400" data-testid="qr-payee">
        收款方 {intent.payee_name ?? intent.payee}
      </p>
      <p className="text-[11px] text-zinc-300">
        意图 {intent.intent_id} · {new Date(intent.expires_at).toLocaleTimeString("zh-CN")} 前有效
      </p>
    </div>
  );
}
