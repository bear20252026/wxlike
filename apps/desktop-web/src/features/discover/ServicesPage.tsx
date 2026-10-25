/**
 * 服务目录 (G11-05 requirement 4): every tile expands to its real target —
 * 目录/报价/下单/退款 or an actual app. Tiles come from the G07 contract;
 * 枚举逐项登记入口 via `tileEntryId` (stable ids registered in
 * integration-requests/G11/G11-05.yaml). Orders created here are queryable
 * by the admin console and other clients through the same ServiceCatalogPort.
 */

import { useCallback, useEffect, useRef, useState } from "react";
import { errorText } from "../../lib/http";
import { formatMoney } from "../../lib/money";
import { DiscoverApi, isUpstreamMissing, orderStateLabel } from "./api";
import type { DiscoverPorts, Quote, ServiceEntry, ServiceOrder } from "./model";
import { ScanPanel } from "./ScanPanel";
import { Banner, EmptyState, GhostButton, PrimaryButton, formatTime } from "../content/ui";

/** Stable entry id per tile: services grid + scan + vision are enumerated. */
export function tileEntryId(entry: ServiceEntry): string {
  return `ENT-SVC-${entry.service_id}`;
}

const DEFAULT_PORTS = new DiscoverApi();

export function ServicesPage({ ports = DEFAULT_PORTS }: { ports?: DiscoverPorts }) {
  const [tiles, setTiles] = useState<ServiceEntry[] | null>(null);
  const [upstream, setUpstream] = useState(false);
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");
  const [detail, setDetail] = useState<ServiceEntry | null>(null);
  const [quote, setQuote] = useState<Quote | null>(null);
  const [orders, setOrders] = useState<ServiceOrder[]>([]);
  const [busy, setBusy] = useState(false);
  const inFlight = useRef(false);

  const refresh = useCallback(async () => {
    try {
      const [list, myOrders] = await Promise.all([ports.services(), ports.orders()]);
      setTiles(list);
      setOrders(myOrders);
      setUpstream(false);
    } catch (err) {
      if (isUpstreamMissing(err)) setUpstream(true);
      else setError(errorText(err));
      setTiles([]);
    }
  }, [ports]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const openTile = async (entry: ServiceEntry) => {
    setError("");
    setDetail(entry);
    setQuote(null);
    if (entry.target.kind === "quote" || entry.target.kind === "catalog") {
      try {
        setQuote(await ports.quote(entry.service_id));
      } catch (err) {
        if (!isUpstreamMissing(err)) setError(errorText(err));
      }
    }
  };

  const placeOrder = async () => {
    if (!detail || !quote) return;
    if (inFlight.current) return; // 写操作重复提交
    inFlight.current = true;
    setBusy(true);
    setError("");
    try {
      const order = await ports.createOrder(detail.service_id, quote.quote_id);
      setOrders((prev) => [order, ...prev]);
      setNotice(`订单已创建：${order.order_id}`);
    } catch (err) {
      setError(errorText(err));
    } finally {
      inFlight.current = false;
      setBusy(false);
    }
  };

  const refund = async (orderId: string) => {
    setBusy(true);
    setError("");
    try {
      const updated = await ports.refundOrder(orderId);
      setOrders((prev) => prev.map((item) => (item.order_id === updated.order_id ? updated : item)));
      setNotice("退款已申请");
    } catch (err) {
      setError(errorText(err));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="mx-auto flex h-full w-full max-w-2xl flex-col gap-3 overflow-y-auto p-4" data-testid="discover-page">
      <Banner tone="error" text={error} />
      <Banner tone="info" text={notice} />
      <Banner tone="info" text={upstream ? "服务目录（G07 ServiceCatalogPort）尚未开通；扫码为真实功能。" : ""} />

      <ScanPanel ports={ports} />

      {tiles !== null && tiles.length === 0 && !upstream ? <EmptyState title="暂无服务" hint="服务目录由管理后台配置" /> : null}

      <div className="grid grid-cols-4 gap-2">
        {(tiles ?? []).map((entry) => (
          <button
            key={entry.service_id}
            type="button"
            onClick={() => void openTile(entry)}
            data-testid={tileEntryId(entry)}
            data-service-id={entry.service_id}
            className="rounded-xl border border-zinc-200 bg-white p-3 text-center transition hover:border-emerald-300 hover:bg-emerald-50"
          >
            <p className="text-2xl">{entry.icon ?? "🧩"}</p>
            <p className="mt-1 truncate text-xs text-zinc-700">{entry.name}</p>
          </button>
        ))}
      </div>

      {detail ? (
        <section className="rounded-xl border border-zinc-200 bg-white p-4" data-testid="service-detail">
          <div className="flex items-center justify-between">
            <h3 className="text-sm font-semibold text-zinc-800">{detail.name}</h3>
            <GhostButton onClick={() => setDetail(null)}>关闭</GhostButton>
          </div>
          <p className="mt-1 text-xs text-zinc-400">
            service_id {detail.service_id} · 目标 {detail.target.kind}
          </p>
          {quote ? (
            <div className="mt-2 rounded-lg bg-zinc-50 p-3 text-sm" data-testid="service-quote">
              <p>
                报价：<strong>{formatMoney(quote.price_minor)}</strong> · {quote.currency}
              </p>
              <p className="mt-0.5 text-xs text-zinc-400">
                报价单 {quote.quote_id} · 有效期至 {new Date(quote.expires_at).toLocaleTimeString("zh-CN")}
                {quote.conditions ? ` · ${quote.conditions}` : ""}
              </p>
              <div className="mt-2">
                <PrimaryButton onClick={() => void placeOrder()} busy={busy}>
                  下单
                </PrimaryButton>
              </div>
            </div>
          ) : null}
          {detail.target.kind === "app" ? (
            <p className="mt-2 text-xs text-zinc-500">该格子指向实际应用：{detail.target.app_id}</p>
          ) : null}
        </section>
      ) : null}

      {orders.length > 0 ? (
        <section className="space-y-1.5">
          <h3 className="text-sm font-semibold text-zinc-700">我的服务订单</h3>
          {orders.map((order) => (
            <div key={order.order_id} className="flex items-center justify-between rounded-lg border border-zinc-200 bg-white px-3 py-2 text-sm" data-testid={`order-${order.order_id}`}>
              <div>
                <p className="font-mono text-xs text-zinc-500">{order.order_id}</p>
                <p className="text-xs text-zinc-400">
                  {order.service_id} · {formatTime(order.updated_at)}
                </p>
              </div>
              <div className="flex items-center gap-2">
                <span
                  className={order.state === "fulfilled" ? "text-emerald-600" : order.state === "failed" ? "text-red-600" : "text-amber-600"}
                  data-testid="order-state"
                >
                  {orderStateLabel(order.state)}
                </span>
                {order.state === "fulfilled" || order.state === "processing" || order.state === "pending_reconciliation" ? (
                  <GhostButton tone="danger" onClick={() => void refund(order.order_id)} disabled={busy}>
                    退款
                  </GhostButton>
                ) : null}
              </div>
            </div>
          ))}
        </section>
      ) : null}
    </div>
  );
}

