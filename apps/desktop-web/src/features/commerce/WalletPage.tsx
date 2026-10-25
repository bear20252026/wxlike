/**
 * 钱包 (G11-04): 零钱/账单/转账/收付款/红包/提现充值/绑卡/卡包.
 *
 * 金额一律整数最小单位（src/lib/money.ts），格式化/解析全程无浮点。
 * 收付款二维码来自短期支付意图（G06 契约），过期即死；支付前展示收款人
 * 与金额确认。未知供应商状态显示「查询中」。虚拟账户（零钱）与真实账户
 * （银行卡/供应商通道）严格区分展示。写操作幂等键 + 在途守卫，并发点击
 * 只产生一笔业务。
 */

import { useCallback, useEffect, useRef, useState } from "react";
import { errorText } from "../../lib/http";
import { formatMoney, parseToMinor } from "../../lib/money";
import { WalletApi, isUpstreamMissing } from "./api";
import type {
  Account,
  EntitlementRecord,
  LedgerPage,
  PaymentIntent,
  PaymentRecord,
  RedpacketRecord,
  WalletPorts,
} from "./model";
import { QrIntentPanel } from "./QrIntent";
import { Banner, EmptyState, GhostButton, Modal, PrimaryButton, formatTime } from "../content/ui";

const UPSTREAM = "支付意图/提现充值/绑卡/卡券（G06 CommercePort）尚未开通";

type Tab = "wallet" | "receive" | "transfer" | "redpacket" | "cards";

/** Module-level default so the `ports` identity is stable across renders
 *  (a per-render `new WalletApi()` would loop the refresh effect and keep
 *  clearing banners). */
const DEFAULT_PORTS = new WalletApi();

export function WalletPage({ ports = DEFAULT_PORTS }: { ports?: WalletPorts }) {
  const [tab, setTab] = useState<Tab>("wallet");
  const [accounts, setAccounts] = useState<Account[] | null>(null);
  const [ledger, setLedger] = useState<LedgerPage | null>(null);
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");

  const refresh = useCallback(async () => {
    setError("");
    try {
      const [accs, page] = await Promise.all([ports.accounts(), ports.ledger(undefined, 50)]);
      setAccounts(accs);
      setLedger(page);
    } catch (err) {
      setError(errorText(err));
    }
  }, [ports]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  return (
    <div className="mx-auto flex h-full w-full max-w-2xl flex-col gap-3 overflow-y-auto p-4" data-testid="wallet-page">
      <div className="flex flex-wrap items-center gap-1.5">
        {(
          [
            ["wallet", "零钱"],
            ["receive", "收付款"],
            ["transfer", "转账"],
            ["redpacket", "红包"],
            ["cards", "银行卡/卡包"],
          ] as const
        ).map(([value, label]) => (
          <button
            key={value}
            type="button"
            onClick={() => setTab(value)}
            data-testid={`wallet-tab-${value}`}
            className={`rounded-lg px-3 py-1.5 text-sm ${tab === value ? "bg-emerald-600 text-white" : "bg-white text-zinc-600 hover:bg-zinc-100"}`}
          >
            {label}
          </button>
        ))}
      </div>

      <Banner tone="error" text={error} />
      <Banner tone="info" text={notice} />

      {tab === "wallet" ? (
        <WalletOverview accounts={accounts} ledger={ledger} onRefresh={() => void refresh()} />
      ) : null}
      {tab === "receive" ? <ReceivePanel ports={ports} onError={setError} onNotice={setNotice} /> : null}
      {tab === "transfer" ? (
        <TransferPanel
          ports={ports}
          onError={setError}
          onNotice={setNotice}
          onDone={() => void refresh()}
        />
      ) : null}
      {tab === "redpacket" ? (
        <RedpacketPanel ports={ports} onError={setError} onNotice={setNotice} onDone={() => void refresh()} />
      ) : null}
      {tab === "cards" ? <CardsPanel ports={ports} onError={setError} /> : null}
    </div>
  );
}

function WalletOverview({
  accounts,
  ledger,
  onRefresh,
}: {
  accounts: Account[] | null;
  ledger: LedgerPage | null;
  onRefresh: () => void;
}) {
  return (
    <section className="space-y-3">
      <div className="grid gap-2 sm:grid-cols-2">
        {(accounts ?? []).map((account) => (
          <div
            key={account.account_id}
            className={`rounded-xl border p-4 ${account.kind === "virtual" ? "border-emerald-200 bg-emerald-50" : "border-zinc-200 bg-white"}`}
            data-testid={`account-${account.account_id}`}
          >
            <p className="text-xs text-zinc-500">
              {account.label}
              <span className={`ml-2 rounded px-1.5 py-0.5 text-[10px] ${account.kind === "virtual" ? "bg-emerald-600 text-white" : "bg-zinc-600 text-white"}`}>
                {account.kind === "virtual" ? "虚拟账户" : "真实账户"}
              </span>
            </p>
            <p className="mt-1 text-2xl font-semibold text-zinc-900" data-testid={`balance-${account.account_id}`}>
              {formatMoney(account.balance_minor)}
            </p>
          </div>
        ))}
        {accounts === null ? <p className="text-sm text-zinc-400" role="status">加载中…</p> : null}
      </div>

      <div className="flex items-center justify-between">
        <h3 className="text-sm font-semibold text-zinc-700">账单</h3>
        <GhostButton onClick={onRefresh}>刷新</GhostButton>
      </div>
      {ledger !== null && ledger.items.length === 0 ? <EmptyState title="暂无账单记录" /> : null}
      <ul className="space-y-1.5">
        {(ledger?.items ?? []).map((row) => (
          <li key={row.id} className="flex items-center justify-between rounded-lg border border-zinc-200 bg-white px-3 py-2 text-sm" data-testid={`ledger-${row.id}`}>
            <div>
              <p className="text-zinc-700">{row.memo || row.kind}</p>
              <p className="text-xs text-zinc-400">
                {row.counterparty} · {formatTime(row.created_at)} · {row.ref_id}
              </p>
            </div>
            <span className={row.amount >= 0 ? "text-emerald-600" : "text-zinc-700"}>
              {row.amount >= 0 ? "+" : ""}
              {formatMoney(row.amount)}
            </span>
          </li>
        ))}
      </ul>
    </section>
  );
}

/** 收付款：短期意图收款码 + 扫码支付（意图契约）。 */
function ReceivePanel({
  ports,
  onError,
  onNotice,
}: {
  ports: WalletPorts;
  onError: (text: string) => void;
  onNotice: (text: string) => void;
}) {
  const [amountInput, setAmountInput] = useState("");
  const [intent, setIntent] = useState<PaymentIntent | null>(null);
  const [busy, setBusy] = useState(false);
  const [scanPayload, setScanPayload] = useState("");
  const [pending, setPending] = useState<PaymentRecord | null>(null);
  const inFlight = useRef(false);
  const upstreamMissing = useRef(false);

  const createIntent = async () => {
    if (inFlight.current) return;
    let minor: bigint;
    try {
      minor = parseToMinor(amountInput);
    } catch (err) {
      
      onError(err instanceof Error ? err.message : "金额不合法");
      return;
    }
    inFlight.current = true;
    setBusy(true);
    onError("");
    try {
      const created = await ports.createReceivingIntent(Number(minor));
      setIntent(created);
      onNotice("收款码已生成，有效期见码面");
    } catch (err) {
      if (isUpstreamMissing(err)) {
        upstreamMissing.current = true;
        onError(`${UPSTREAM}（收款码不可用）`);
      } else {
        onError(errorText(err));
      }
    } finally {
      inFlight.current = false;
      setBusy(false);
    }
  };

  const pay = async () => {
    if (inFlight.current || !scanPayload.trim()) return;
    inFlight.current = true;
    setBusy(true);
    onError("");
    try {
      const record = await ports.payIntent(scanPayload.trim());
      setPending(record);
      if (record.state === "succeeded") onNotice("支付成功");
      else if (record.state === "querying") onNotice("支付状态查询中，确认后自动更新");
    } catch (err) {
      if (isUpstreamMissing(err)) onError(`${UPSTREAM}（扫码支付不可用）`);
      else onError(errorText(err));
    } finally {
      inFlight.current = false;
      setBusy(false);
    }
  };

  return (
    <section className="space-y-3">
      <div className="rounded-xl border border-zinc-200 bg-white p-4">
        <h3 className="text-sm font-semibold text-zinc-700">收款</h3>
        <div className="mt-2 flex items-end gap-2">
          <label className="flex-1 text-xs text-zinc-500">
            金额（元，最多两位小数）
            <input
              value={amountInput}
              onChange={(event) => setAmountInput(event.target.value)}
              inputMode="decimal"
              placeholder="0.00"
              data-testid="receive-amount"
              className="mt-1 w-full rounded-lg border border-zinc-200 px-3 py-2 text-sm outline-none focus:border-emerald-500"
            />
          </label>
          <PrimaryButton onClick={() => void createIntent()} busy={busy} disabled={!amountInput.trim()}>
            生成收款码
          </PrimaryButton>
        </div>
        {intent ? (
          <div className="mt-3 flex justify-center">
            <QrIntentPanel
              intent={intent}
              refreshing={busy}
              onRefresh={() => void createIntent()}
              onExpired={() => onNotice("收款码已过期，请重新生成")}
            />
          </div>
        ) : null}
      </div>

      <div className="rounded-xl border border-zinc-200 bg-white p-4">
        <h3 className="text-sm font-semibold text-zinc-700">向商家付款（粘贴/输入二维码内容）</h3>
        <div className="mt-2 flex items-end gap-2">
          <input
            value={scanPayload}
            onChange={(event) => setScanPayload(event.target.value)}
            placeholder="wxpay:intent:…"
            data-testid="pay-payload"
            className="flex-1 rounded-lg border border-zinc-200 px-3 py-2 font-mono text-xs outline-none focus:border-emerald-500"
          />
          <PrimaryButton onClick={() => void pay()} busy={busy} disabled={!scanPayload.trim()}>
            确认支付
          </PrimaryButton>
        </div>
        {pending ? <PaymentStateCard record={pending} ports={ports} onError={onError} /> : null}
      </div>
    </section>
  );
}

function PaymentStateCard({
  record,
  ports,
  onError,
}: {
  record: PaymentRecord;
  ports: WalletPorts;
  onError: (text: string) => void;
}) {
  const [state, setState] = useState(record);
  const [busy, setBusy] = useState(false);
  const label =
    state.state === "succeeded"
      ? "支付成功"
      : state.state === "querying" || state.state === "pending"
        ? "查询中…"
        : state.state === "refund_pending"
          ? "退款处理中"
          : state.state === "refunded"
            ? "已退款"
            : "支付失败";

  const requery = async () => {
    setBusy(true);
    try {
      setState(await ports.paymentState(state.payment_id));
    } catch (err) {
      onError(errorText(err));
    } finally {
      setBusy(false);
    }
  };

  const doRefund = async () => {
    setBusy(true);
    try {
      setState(await ports.refund(state.payment_id));
    } catch (err) {
      onError(errorText(err));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="mt-3 rounded-lg border border-zinc-200 p-3 text-sm" data-testid={`payment-${state.payment_id}`}>
      <div className="flex items-center justify-between">
        <span>{formatMoney(state.amount_minor)}</span>
        <span
          data-testid="payment-state"
          className={state.state === "succeeded" ? "text-emerald-600" : state.state === "failed" ? "text-red-600" : "text-amber-600"}
        >
          {label}
        </span>
      </div>
      <p className="mt-1 text-xs text-zinc-400">
        {state.account_kind === "virtual" ? "虚拟账户" : "真实账户"} · {state.payment_id}
      </p>
      <div className="mt-2 flex gap-2">
        {state.state === "querying" || state.state === "pending" || state.state === "refund_pending" ? (
          <GhostButton onClick={() => void requery()} disabled={busy}>
            再次查询
          </GhostButton>
        ) : null}
        {state.state === "succeeded" || state.state === "failed" ? (
          <GhostButton tone="danger" onClick={() => void doRefund()} disabled={busy}>
            申请退款
          </GhostButton>
        ) : null}
      </div>
    </div>
  );
}

/** 转账：确认协议（展示收款人/金额）→ 幂等执行；并发点击仅一笔。 */
function TransferPanel({
  ports,
  onError,
  onNotice,
  onDone,
}: {
  ports: WalletPorts;
  onError: (text: string) => void;
  onNotice: (text: string) => void;
  onDone: () => void;
}) {
  const [toUid, setToUid] = useState("");
  const [amountInput, setAmountInput] = useState("");
  const [memo, setMemo] = useState("");
  const [confirming, setConfirming] = useState(false);
  const [busy, setBusy] = useState(false);
  const inFlight = useRef(false);

  const openConfirm = () => {
    onError("");
    try {
      parseToMinor(amountInput);
    } catch (err) {
      onError(err instanceof Error ? err.message : "金额不合法");
      return;
    }
    if (!toUid.trim()) {
      onError("请填写对方用户 ID");
      return;
    }
    setConfirming(true); // confirmation protocol：先展示，再执行
  };

  const execute = async () => {
    if (inFlight.current) return; // 并发点击仅一笔业务
    inFlight.current = true;
    setBusy(true);
    onError("");
    try {
      const minor = parseToMinor(amountInput);
      await ports.transfer({ to_uid: toUid.trim(), amount_minor: Number(minor), memo: memo.trim() || undefined });
      setConfirming(false);
      setToUid("");
      setAmountInput("");
      setMemo("");
      onNotice("转账已提交");
      onDone();
    } catch (err) {
      onError(errorText(err));
    } finally {
      inFlight.current = false;
      setBusy(false);
    }
  };

  return (
    <section className="rounded-xl border border-zinc-200 bg-white p-4">
      <div className="space-y-2">
        <label className="block text-xs text-zinc-500">
          对方用户 ID
          <input
            value={toUid}
            onChange={(event) => setToUid(event.target.value)}
            data-testid="transfer-to"
            className="mt-1 w-full rounded-lg border border-zinc-200 px-3 py-2 text-sm outline-none focus:border-emerald-500"
          />
        </label>
        <label className="block text-xs text-zinc-500">
          金额（元）
          <input
            value={amountInput}
            onChange={(event) => setAmountInput(event.target.value)}
            inputMode="decimal"
            placeholder="0.00"
            data-testid="transfer-amount"
            className="mt-1 w-full rounded-lg border border-zinc-200 px-3 py-2 text-sm outline-none focus:border-emerald-500"
          />
        </label>
        <label className="block text-xs text-zinc-500">
          备注（可选）
          <input
            value={memo}
            onChange={(event) => setMemo(event.target.value)}
            maxLength={60}
            data-testid="transfer-memo"
            className="mt-1 w-full rounded-lg border border-zinc-200 px-3 py-2 text-sm outline-none focus:border-emerald-500"
          />
        </label>
        <PrimaryButton onClick={openConfirm} title="转账">
          转账
        </PrimaryButton>
      </div>

      {confirming ? (
        <Modal title="确认转账" onClose={() => setConfirming(false)}>
          <div className="space-y-3" data-testid="transfer-confirm">
            <p className="text-sm text-zinc-600">
              收款人 <strong data-testid="confirm-payee">{toUid.trim()}</strong>
            </p>
            <p className="text-2xl font-semibold text-zinc-900" data-testid="confirm-amount">
              {safeFormat(amountInput)}
            </p>
            <p className="text-xs text-zinc-400">请确认收款人与金额；提交后通过幂等键保证只执行一次。</p>
            <div className="flex justify-end gap-2">
              <GhostButton onClick={() => setConfirming(false)}>取消</GhostButton>
              <PrimaryButton onClick={() => void execute()} busy={busy}>
                确认转账
              </PrimaryButton>
            </div>
          </div>
        </Modal>
      ) : null}
    </section>
  );
}

/** 红包：创建/领取走真实端点；领取幂等。 */
function RedpacketPanel({
  ports,
  onError,
  onNotice,
  onDone,
}: {
  ports: WalletPorts;
  onError: (text: string) => void;
  onNotice: (text: string) => void;
  onDone: () => void;
}) {
  const [amountInput, setAmountInput] = useState("");
  const [count, setCount] = useState(1);
  const [memo, setMemo] = useState("");
  const [claimId, setClaimId] = useState("");
  const [claimed, setClaimed] = useState<number | null>(null);
  const [busy, setBusy] = useState(false);
  const inFlight = useRef(false);

  const create = async () => {
    if (inFlight.current) return;
    let minor: bigint;
    try {
      minor = parseToMinor(amountInput);
    } catch (err) {
      onError(err instanceof Error ? err.message : "金额不合法");
      return;
    }
    if (count < 1 || Number(minor) < count) {
      onError("红包总额需足够拆分为每份至少 0.01 元");
      return;
    }
    inFlight.current = true;
    setBusy(true);
    onError("");
    try {
      const record: RedpacketRecord = await ports.createRedpacket({
        amount_minor: Number(minor),
        count,
        memo: memo.trim() || undefined,
      });
      onNotice(`红包已创建：${record.redpacket_id}`);
      setAmountInput("");
      onDone();
    } catch (err) {
      onError(errorText(err));
    } finally {
      inFlight.current = false;
      setBusy(false);
    }
  };

  const claim = async () => {
    if (inFlight.current || !claimId.trim()) return;
    inFlight.current = true;
    setBusy(true);
    onError("");
    try {
      const reply = await ports.claimRedpacket(claimId.trim());
      setClaimed(reply.amount);
      onNotice("领取成功");
      onDone();
    } catch (err) {
      setClaimed(null);
      onError(errorText(err));
    } finally {
      inFlight.current = false;
      setBusy(false);
    }
  };

  return (
    <section className="space-y-3">
      <div className="rounded-xl border border-zinc-200 bg-white p-4">
        <h3 className="text-sm font-semibold text-zinc-700">发红包</h3>
        <div className="mt-2 grid grid-cols-3 gap-2">
          <input
            value={amountInput}
            onChange={(event) => setAmountInput(event.target.value)}
            inputMode="decimal"
            placeholder="总额（元）"
            data-testid="redpacket-amount"
            className="rounded-lg border border-zinc-200 px-3 py-2 text-sm outline-none focus:border-emerald-500"
          />
          <input
            value={String(count)}
            onChange={(event) => setCount(Math.max(1, Math.min(1000, Number(event.target.value) || 1)))}
            inputMode="numeric"
            placeholder="个数"
            data-testid="redpacket-count"
            className="rounded-lg border border-zinc-200 px-3 py-2 text-sm outline-none focus:border-emerald-500"
          />
          <input
            value={memo}
            onChange={(event) => setMemo(event.target.value)}
            placeholder="祝福语（可选）"
            maxLength={60}
            data-testid="redpacket-memo"
            className="rounded-lg border border-zinc-200 px-3 py-2 text-sm outline-none focus:border-emerald-500"
          />
        </div>
        <div className="mt-2">
          <PrimaryButton onClick={() => void create()} busy={busy} disabled={!amountInput.trim()}>
            塞钱进红包
          </PrimaryButton>
        </div>
      </div>

      <div className="rounded-xl border border-zinc-200 bg-white p-4">
        <h3 className="text-sm font-semibold text-zinc-700">领红包</h3>
        <div className="mt-2 flex items-end gap-2">
          <input
            value={claimId}
            onChange={(event) => setClaimId(event.target.value)}
            placeholder="红包 ID"
            data-testid="redpacket-claim-id"
            className="flex-1 rounded-lg border border-zinc-200 px-3 py-2 font-mono text-xs outline-none focus:border-emerald-500"
          />
          <PrimaryButton onClick={() => void claim()} busy={busy} disabled={!claimId.trim()}>
            领取
          </PrimaryButton>
        </div>
        {claimed !== null ? (
          <p className="mt-2 text-sm text-emerald-600" data-testid="redpacket-claimed">
            已领取 {formatMoney(claimed)}
          </p>
        ) : null}
      </div>
    </section>
  );
}

/** 绑卡/卡包：G06 契约面。提现/充值入口在此（上游未开通时明确提示）。 */
function CardsPanel({ ports, onError }: { ports: WalletPorts; onError: (text: string) => void }) {
  const [cards, setCards] = useState<import("./model").CardRecord[] | null>(null);
  const [entitlements, setEntitlements] = useState<EntitlementRecord[] | null>(null);
  const [upstream, setUpstream] = useState(false);
  const [withdrawAccount, setWithdrawAccount] = useState("");
  const [withdrawAmount, setWithdrawAmount] = useState("");
  const [busy, setBusy] = useState(false);
  const inFlight = useRef(false);

  useEffect(() => {
    let alive = true;
    void (async () => {
      try {
        const [cardList, ents] = await Promise.all([ports.cards(), ports.entitlements()]);
        if (!alive) return;
        setCards(cardList);
        setEntitlements(ents);
      } catch (err) {
        if (!alive) return;
        if (isUpstreamMissing(err)) setUpstream(true);
        else onError(errorText(err));
        setCards([]);
        setEntitlements([]);
      }
    })();
    return () => {
      alive = false;
    };
  }, [ports, onError]);

  const withdraw = async () => {
    if (inFlight.current) return;
    let minor: bigint;
    try {
      minor = parseToMinor(withdrawAmount);
    } catch (err) {
      onError(err instanceof Error ? err.message : "金额不合法");
      return;
    }
    inFlight.current = true;
    setBusy(true);
    try {
      const record = await ports.withdraw(withdrawAccount.trim(), Number(minor));
      onError("");
      if (record.state === "querying" || record.state === "pending") {
        onError("提现状态查询中，可在账单中跟踪；失败后可重试");
      }
    } catch (err) {
      if (isUpstreamMissing(err)) onError(`${UPSTREAM}（提现不可用）`);
      else onError(errorText(err));
    } finally {
      inFlight.current = false;
      setBusy(false);
    }
  };

  return (
    <section className="space-y-3">
      <Banner tone="info" text={upstream ? `${UPSTREAM}；提现/绑卡/卡券在 G06 合入前不可用。` : ""} />
      <div className="rounded-xl border border-zinc-200 bg-white p-4">
        <h3 className="text-sm font-semibold text-zinc-700">提现（虚拟账户 → 真实账户）</h3>
        <div className="mt-2 flex items-end gap-2">
          <input
            value={withdrawAccount}
            onChange={(event) => setWithdrawAccount(event.target.value)}
            placeholder="目标银行账户 ID"
            data-testid="withdraw-account"
            className="flex-1 rounded-lg border border-zinc-200 px-3 py-2 font-mono text-xs outline-none focus:border-emerald-500"
          />
          <input
            value={withdrawAmount}
            onChange={(event) => setWithdrawAmount(event.target.value)}
            inputMode="decimal"
            placeholder="金额（元）"
            data-testid="withdraw-amount"
            className="w-32 rounded-lg border border-zinc-200 px-3 py-2 text-sm outline-none focus:border-emerald-500"
          />
          <PrimaryButton onClick={() => void withdraw()} busy={busy} disabled={!withdrawAccount.trim() || !withdrawAmount.trim()}>
            申请提现
          </PrimaryButton>
        </div>
      </div>

      <div className="rounded-xl border border-zinc-200 bg-white p-4">
        <h3 className="text-sm font-semibold text-zinc-700">银行卡</h3>
        {(cards ?? []).length === 0 ? (
          <p className="mt-2 text-xs text-zinc-400">{upstream ? "上游未开通" : "暂无绑定的银行卡"}</p>
        ) : null}
        <ul className="mt-2 space-y-1.5">
          {(cards ?? []).map((card) => (
            <li key={card.card_id} className="flex items-center justify-between rounded-lg border border-zinc-200 px-3 py-2 text-sm">
              <span>
                {card.label} ···· {card.masked_number}
              </span>
              <span className={`rounded px-1.5 py-0.5 text-[10px] text-white ${card.kind === "real" ? "bg-zinc-600" : "bg-emerald-600"}`}>
                {card.kind === "real" ? "真实账户" : "虚拟账户"}
              </span>
            </li>
          ))}
        </ul>
      </div>

      <div className="rounded-xl border border-zinc-200 bg-white p-4">
        <h3 className="text-sm font-semibold text-zinc-700">卡包</h3>
        {(entitlements ?? []).length === 0 ? (
          <p className="mt-2 text-xs text-zinc-400">{upstream ? "上游未开通" : "暂无卡券票证"}</p>
        ) : null}
        <ul className="mt-2 space-y-1.5">
          {(entitlements ?? []).map((item) => (
            <li key={item.entitlement_id} className="rounded-lg border border-zinc-200 px-3 py-2 text-sm">
              <div className="flex items-center justify-between">
                <span>{item.title}</span>
                <span className={item.status === "valid" ? "text-emerald-600" : "text-zinc-400"}>
                  {item.status === "valid" ? "可用" : item.status === "used" ? "已使用" : "已过期"}
                </span>
              </div>
              <p className="mt-0.5 text-xs text-zinc-400">
                {item.issuer} · {new Date(item.expires_at).toLocaleDateString("zh-CN")} 到期
              </p>
            </li>
          ))}
        </ul>
      </div>
    </section>
  );
}

function safeFormat(amountInput: string): string {
  try {
    return formatMoney(parseToMinor(amountInput));
  } catch {
    return "—";
  }
}
