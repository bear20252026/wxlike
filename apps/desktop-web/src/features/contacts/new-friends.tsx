import { useCallback, useEffect, useRef, useState } from "react";
import { ArrowLeft, Check, Inbox, MessageSquare, X } from "lucide-react";
import { ACTIONS, type DomainError } from "../../shell/core-contract";
import type { FriendRequest } from "../../shell/models";
import { useAppState, useCoreAction, useToast } from "../../shell/store";
import { navigate } from "../../shell/router";
import { Avatar, Button, EmptyState, ErrorState, LoadingState, cn } from "../../shell/ui";

export function NewFriendsPage() {
  const state = useAppState();
  const dispatch = useCoreAction();
  const toast = useToast();
  const [requests, setRequests] = useState<FriendRequest[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<DomainError | null>(null);
  const [pending, setPending] = useState<Record<string, "accept" | "decline">>({});
  const mounted = useRef(true);
  const inFlight = useRef(new Set<string>());

  const load = useCallback(async () => {
    setLoading(true);
    const response = await dispatch<FriendRequest[]>(ACTIONS.requestsIncoming, {}, { silent: true });
    if (!mounted.current) return;
    if (response.ok) {
      setRequests(response.data);
      setError(null);
    } else {
      setError(response.error);
    }
    setLoading(false);
  }, [dispatch]);

  useEffect(() => {
    mounted.current = true;
    void load();
    return () => {
      mounted.current = false;
    };
  }, [load]);

  useEffect(() => {
    if (!state.requests.length) return;
    setRequests((rows) => {
      const map = new Map(rows.map((row) => [row.request_id, row]));
      for (const row of state.requests) map.set(row.request_id, row);
      return [...map.values()].sort((a, b) => b.created_at_ms.localeCompare(a.created_at_ms));
    });
  }, [state.requests]);

  const resolve = async (request: FriendRequest, mode: "accept" | "decline") => {
    if (inFlight.current.has(request.request_id)) return;
    inFlight.current.add(request.request_id);
    setPending((map) => ({ ...map, [request.request_id]: mode }));
    const action = mode === "accept" ? ACTIONS.requestAccept : ACTIONS.requestDecline;
    const response = await dispatch(action, { request_id: request.request_id }, {
      idempotencyKey: `${mode}:${request.request_id}`,
    });
    inFlight.current.delete(request.request_id);
    setPending((map) => {
      const next = { ...map };
      delete next[request.request_id];
      return next;
    });
    if (response.ok) toast(mode === "accept" ? `已添加 ${request.from.name} 为好友` : "已拒绝该申请");
  };

  const pendingCount = requests.filter((item) => item.status === "PENDING").length;

  return (
    <div className="flex min-h-0 flex-1 flex-col bg-[var(--surface)]">
      <header className="flex h-[52px] shrink-0 items-center gap-2 border-b border-[var(--line)] bg-[var(--surface-2)] px-4">
        <button
          type="button"
          onClick={() => navigate({ id: "contacts" })}
          className="flex items-center gap-1 text-[10.5px] text-[var(--muted)] hover:text-[var(--ink)]"
          data-testid="new-friends-back"
        >
          <ArrowLeft size={13} /> 通讯录
        </button>
        <h1 className="ml-2 text-[13px] font-semibold text-[var(--ink)]">新的朋友</h1>
        <span className="text-[9.5px] text-[var(--muted)]">{pendingCount} 条待处理</span>
        <Button size="sm" className="ml-auto" onClick={() => navigate({ id: "contacts" })}>
          添加朋友
        </Button>
      </header>

      <div className="scrollbar-thin min-h-0 flex-1 overflow-y-auto px-5 py-4">
        {loading ? (
          <LoadingState label="正在读取好友申请…" />
        ) : error ? (
          <ErrorState error={error} onRetry={() => void load()} />
        ) : requests.length === 0 ? (
          <EmptyState
            icon={<Inbox size={20} />}
            title="没有新的朋友申请"
            description="对方提交申请后由服务端推送；接受后双方关系与搜索结果会同步刷新。"
          />
        ) : (
          <ul className="mx-auto max-w-[720px] space-y-2">
            {requests.map((request) => {
              const action = pending[request.request_id];
              const done = request.status !== "PENDING";
              return (
                <li
                  key={request.request_id}
                  className="flex items-center gap-3 rounded-lg border border-[var(--line-strong)] bg-[var(--surface-2)] px-3 py-2.5"
                  data-testid={`friend-request-${request.request_id}`}
                >
                  <Avatar src={request.from.avatar} name={request.from.name} size={38} />
                  <div className="min-w-0 flex-1">
                    <div className="flex items-baseline gap-2">
                      <span className="text-[12px] font-medium text-[var(--ink)]">{request.from.name}</span>
                      <span className="text-[9px] text-[var(--muted)]">ID {request.from.user_id}</span>
                      <span
                        className={cn(
                          "rounded px-1.5 py-0.5 text-[8.5px]",
                          request.status === "ACCEPTED"
                            ? "bg-[var(--accent-soft)] text-[var(--accent-ink)]"
                            : request.status === "DECLINED"
                              ? "bg-[var(--surface-3)] text-[var(--muted)]"
                              : "bg-[var(--warn)]/12 text-[var(--warn)]",
                        )}
                      >
                        {request.status === "ACCEPTED" ? "已接受" : request.status === "DECLINED" ? "已拒绝" : "待处理"}
                      </span>
                    </div>
                    <p className="mt-0.5 truncate text-[10px] text-[var(--ink-2)]">{request.message || "请求添加你为好友"}</p>
                    <p className="mt-0.5 text-[8.5px] text-[var(--muted)]">{new Date(request.created_at_ms).toLocaleString()}</p>
                  </div>
                  <div className="flex shrink-0 items-center gap-1.5">
                    {done && request.status === "ACCEPTED" ? (
                      <Button
                        size="sm"
                        onClick={() => navigate({ id: "contact", userId: request.from.user_id })}
                        data-testid={`friend-open-${request.request_id}`}
                      >
                        <MessageSquare size={11} /> 资料
                      </Button>
                    ) : null}
                    <Button
                      size="sm"
                      variant="primary"
                      disabled={done}
                      pending={action === "accept"}
                      onClick={() => void resolve(request, "accept")}
                      data-testid={`friend-accept-${request.request_id}`}
                    >
                      <Check size={11} /> 接受
                    </Button>
                    <Button
                      size="sm"
                      variant="ghost"
                      disabled={done}
                      pending={action === "decline"}
                      onClick={() => void resolve(request, "decline")}
                      data-testid={`friend-decline-${request.request_id}`}
                    >
                      <X size={11} /> 拒绝
                    </Button>
                  </div>
                </li>
              );
            })}
          </ul>
        )}
      </div>
      <div className="flex h-7 shrink-0 items-center justify-between border-t border-[var(--line)] px-4 text-[9px] text-[var(--muted)]">
        <span>{requests.length} 条申请</span>
        <span>重复点击会被锁定，状态以服务端返回为准</span>
      </div>
    </div>
  );
}
