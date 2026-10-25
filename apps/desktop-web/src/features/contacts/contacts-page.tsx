import { useCallback, useEffect, useRef, useState } from "react";
import { Search, Star, UserPlus, Users, X } from "lucide-react";
import { ACTIONS, type DomainError } from "../../shell/core-contract";
import type { Contact } from "../../shell/models";
import { useAppState, useCoreAction, useToast } from "../../shell/store";
import { useDebounced } from "../../shell/hooks";
import { navigate } from "../../shell/router";
import { Avatar, Badge, Button, Dialog, EmptyState, ErrorState, InlineError, LoadingState, TextField, cn } from "../../shell/ui";
import { usePending } from "../../shell/hooks";
import { ContactDetail } from "./contact-detail";

export function ContactsPage({ userId }: { userId?: string }) {
  const state = useAppState();
  const dispatch = useCoreAction();
  const [query, setQuery] = useState("");
  const debounced = useDebounced(query, 200);
  const [tag, setTag] = useState<string | null>(null);
  const [starOnly, setStarOnly] = useState(false);
  const [contacts, setContacts] = useState<Contact[]>([]);
  const [tags, setTags] = useState<string[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<DomainError | null>(null);
  const [addOpen, setAddOpen] = useState(false);
  const ticket = useRef(0);

  const load = useCallback(async () => {
    const current = ++ticket.current;
    setLoading(true);
    setError(null);
    const response = await dispatch<{ items: Contact[]; tags: string[] }>(
      ACTIONS.relationsList,
      { query: debounced.trim(), tag, star: starOnly },
      { silent: true },
    );
    if (current !== ticket.current) return;
    if (response.ok) {
      setContacts(response.data.items);
      setTags(response.data.tags);
    } else {
      setError(response.error);
    }
    setLoading(false);
  }, [dispatch, debounced, tag, starOnly]);

  useEffect(() => {
    void load();
  }, [load]);

  useEffect(() => {
    if (state.contacts.length) void load();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [state.contacts.length]);

  const pending = state.requests.filter((item) => item.status === "PENDING").length;

  return (
    <div className="flex min-h-0 flex-1">
      <aside className="flex w-[300px] shrink-0 flex-col border-r border-[var(--line)] bg-[var(--surface)]">
        <div className="shrink-0 px-3 pb-2 pt-3">
          <div className="flex items-center justify-between">
            <h1 className="text-[15px] font-semibold text-[var(--ink)]">通讯录</h1>
            <div className="flex items-center gap-1">
              <button
                type="button"
                onClick={() => navigate({ id: "new_friends" })}
                className="flex h-7 items-center gap-1 rounded-md px-1.5 text-[9.5px] text-[var(--ink-2)] hover:bg-[var(--surface-3)]"
                data-testid="contacts-new-friends"
              >
                <Users size={13} /> 新的朋友
                {pending > 0 ? <Badge count={pending} /> : null}
              </button>
              <button
                type="button"
                onClick={() => setAddOpen(true)}
                title="添加朋友"
                className="grid h-7 w-7 place-items-center rounded-md text-[var(--ink-2)] hover:bg-[var(--surface-3)]"
                data-testid="contacts-add"
              >
                <UserPlus size={15} />
              </button>
            </div>
          </div>
          <div className="mt-2 flex h-[30px] items-center gap-1.5 rounded-md bg-[var(--surface-3)] px-2.5">
            <Search size={12} className="text-[var(--muted)]" />
            <input
              value={query}
              onChange={(event) => setQuery(event.target.value)}
              placeholder="搜索姓名、备注或标签"
              aria-label="搜索联系人"
              className="w-full bg-transparent text-[11px] outline-none placeholder:text-[var(--muted)]"
              data-testid="contacts-search"
            />
            {query ? (
              <button type="button" aria-label="清空" onClick={() => setQuery("")} className="text-[var(--muted)]">
                <X size={11} />
              </button>
            ) : null}
          </div>
          <div className="mt-2 flex flex-wrap items-center gap-1">
            <button
              type="button"
              onClick={() => { setTag(null); setStarOnly(false); }}
              className={cn("rounded-full px-2 py-0.5 text-[9px]", !tag && !starOnly ? "bg-[var(--ink)] text-[var(--app-bg)]" : "text-[var(--muted)] hover:bg-[var(--surface-3)]")}
              data-testid="contacts-tag-all"
            >
              全部
            </button>
            <button
              type="button"
              onClick={() => { setStarOnly((value) => !value); setTag(null); }}
              className={cn("flex items-center gap-0.5 rounded-full px-2 py-0.5 text-[9px]", starOnly ? "bg-[var(--warn)]/15 text-[var(--warn)]" : "text-[var(--muted)] hover:bg-[var(--surface-3)]")}
              data-testid="contacts-star-filter"
            >
              <Star size={9} /> 星标
            </button>
            {tags.map((item) => (
              <button
                key={item}
                type="button"
                onClick={() => { setTag(item === tag ? null : item); setStarOnly(false); }}
                className={cn("rounded-full px-2 py-0.5 text-[9px]", tag === item ? "bg-[var(--accent-soft)] text-[var(--accent-ink)]" : "text-[var(--muted)] hover:bg-[var(--surface-3)]")}
                data-testid={`contacts-tag-${item}`}
              >
                {item}
              </button>
            ))}
          </div>
        </div>

        <div className="scrollbar-thin min-h-0 flex-1 overflow-y-auto px-1.5 pb-2" data-testid="contacts-list">
          {loading ? (
            <LoadingState label="正在读取联系人…" />
          ) : error ? (
            <ErrorState error={error} compact onRetry={() => void load()} />
          ) : contacts.length === 0 ? (
            <EmptyState
              title={debounced || tag || starOnly ? "没有匹配的联系人" : "还没有联系人"}
              description="联系人由服务端关系链返回，接受好友申请后会出现在这里。"
              action={<Button size="sm" onClick={() => navigate({ id: "new_friends" })}>查看新的朋友</Button>}
            />
          ) : (
            contacts.map((contact) => (
              <button
                key={contact.user_id}
                type="button"
                onClick={() => navigate({ id: "contact", userId: contact.user_id })}
                className={cn(
                  "flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left transition",
                  userId === contact.user_id ? "bg-[var(--surface-3)]" : "hover:bg-[var(--surface-3)]/60",
                )}
                data-testid={`contact-${contact.user_id}`}
              >
                <Avatar src={contact.avatar} name={contact.name} size={32} online={contact.online} />
                <span className="min-w-0 flex-1">
                  <span className="flex items-center gap-1 text-[11.5px] font-medium text-[var(--ink)]">
                    <span className="truncate">{contact.name}</span>
                    {contact.star ? <Star size={9} className="shrink-0 text-[var(--warn)]" /> : null}
                  </span>
                  <span className="block truncate text-[9px] text-[var(--muted)]">
                    {contact.note || contact.region || contact.user_id}
                    {contact.tags.length ? ` · ${contact.tags.join(" / ")}` : ""}
                  </span>
                </span>
              </button>
            ))
          )}
        </div>
        <div className="flex h-7 shrink-0 items-center justify-between border-t border-[var(--line)] px-3 text-[9px] text-[var(--muted)]">
          <span>{contacts.length} 位联系人</span>
          <span>服务端关系链</span>
        </div>
      </aside>

      {userId ? (
        <ContactDetail key={userId} userId={userId} />
      ) : (
        <section className="flex min-h-0 min-w-0 flex-1 items-center justify-center bg-[var(--surface)]">
          <EmptyState icon={<Users size={20} />} title="选择一位联系人" description="查看资料、共同群聊、备注、标签与拉黑状态；全部操作以服务端鉴权为准。" />
        </section>
      )}

      <AddFriendDialog open={addOpen} onClose={() => setAddOpen(false)} />
    </div>
  );
}

function AddFriendDialog({ open, onClose }: { open: boolean; onClose: () => void }) {
  const dispatch = useCoreAction();
  const toast = useToast();
  const { pending, run } = usePending();
  const [userId, setUserId] = useState("");
  const [message, setMessage] = useState("");
  const [errors, setErrors] = useState<Record<string, string>>({});
  const [error, setError] = useState<DomainError | null>(null);

  useEffect(() => {
    if (!open) {
      setUserId("");
      setMessage("");
      setErrors({});
      setError(null);
    }
  }, [open]);

  const submit = async () => {
    const next: Record<string, string> = {};
    if (!userId.trim()) next.user_id = "请输入用户 ID";
    if (message.length > 50) next.message = "附言最多 50 字";
    setErrors(next);
    setError(null);
    if (Object.keys(next).length) return;
    await run(async () => {
      const response = await dispatch(ACTIONS.requestSend, { user_id: userId.trim(), message: message.trim() }, {
        idempotencyKey: `friend:${userId.trim()}:${message.trim()}`,
      });
      if (response.ok) {
        toast("好友申请已提交，等待对方处理");
        onClose();
      } else {
        setErrors(response.error.field_errors ?? {});
        setError(response.error.field_errors ? null : response.error);
      }
    });
  };

  return (
    <Dialog
      open={open}
      title="添加朋友"
      description="使用真实用户 ID 发起申请；按钮在请求完成前锁定，重复点击不会重复提交。"
      onClose={onClose}
      footer={
        <>
          <Button size="sm" onClick={onClose} disabled={pending}>
            取消
          </Button>
          <Button size="sm" variant="primary" pending={pending} onClick={() => void submit()} data-testid="add-friend-submit">
            发送申请
          </Button>
        </>
      }
    >
      <div className="space-y-2">
        <TextField
          label="用户 ID"
          value={userId}
          onChange={(event) => setUserId(event.target.value)}
          placeholder="例如 user-yuran"
          error={errors.user_id ?? errors.target}
          data-testid="add-friend-id"
        />
        <TextField
          label="附言"
          value={message}
          onChange={(event) => setMessage(event.target.value)}
          placeholder="介绍一下自己（最多 50 字）"
          error={errors.message}
          hint={`${message.length}/50`}
          data-testid="add-friend-message"
        />
        <InlineError error={error} />
      </div>
    </Dialog>
  );
}
