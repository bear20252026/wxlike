import { useEffect, useState } from "react";
import { Ban, MessageSquare, Star, UserPlus } from "lucide-react";
import { ACTIONS } from "../../shell/core-contract";
import type { RelationProfile } from "../../shell/models";
import { useAppState, useCoreAction, useToast } from "../../shell/store";
import { useAsync, usePending } from "../../shell/hooks";
import { navigate } from "../../shell/router";
import { Avatar, Button, ConfirmDialog, ErrorState, LoadingState, TextField } from "../../shell/ui";

export function ContactDetail({ userId }: { userId: string }) {
  const state = useAppState();
  const dispatch = useCoreAction();
  const toast = useToast();
  const { pending, run } = usePending();
  const profileState = useAsync<RelationProfile>(() => dispatch(ACTIONS.relationProfile, { user_id: userId }), [userId]);
  const contact = state.contacts.find((row) => row.user_id === userId);
  const [note, setNote] = useState("");
  const [noteError, setNoteError] = useState<string | null>(null);
  const [tags, setTags] = useState("");
  const [tagError, setTagError] = useState<string | null>(null);
  const [blockOpen, setBlockOpen] = useState(false);

  const profile = profileState.data;
  useEffect(() => {
    setNote(contact?.note ?? profile?.note ?? "");
    setTags((contact?.tags ?? []).join(", "));
  }, [contact?.note, contact?.tags, profile?.note]);

  const openChat = async () => {
    const response = await dispatch<{ conv_id: string }>(ACTIONS.conversationEnsure, { user_id: userId });
    if (response.ok) navigate({ id: "chat", convId: response.data.conv_id });
  };

  const toggleStar = async () => {
    if (!contact) return;
    await run(async () => {
      await dispatch(ACTIONS.relationStar, { user_id: userId, star: !contact.star }, {
        idempotencyKey: `star:${userId}:${!contact.star}`,
      });
    });
  };

  const saveNote = async () => {
    if (note.trim().length > 32) {
      setNoteError("备注最多 32 字");
      return;
    }
    setNoteError(null);
    await run(async () => {
      const response = await dispatch(ACTIONS.relationNote, { user_id: userId, note: note.trim() }, {
        idempotencyKey: `note:${userId}:${note.trim()}`,
      });
      if (response.ok) {
        toast("备注已保存");
        profileState.reload();
      } else if (response.error.field_errors?.note) {
        setNoteError(response.error.field_errors.note);
      }
    });
  };

  const saveTags = async () => {
    const list = tags.split(/[,，\s]+/).map((item) => item.trim()).filter(Boolean);
    if (list.length > 10) {
      setTagError("标签最多 10 个");
      return;
    }
    setTagError(null);
    await run(async () => {
      const response = await dispatch(ACTIONS.relationTag, { user_id: userId, tags: list }, {
        idempotencyKey: `tags:${userId}:${list.join(",")}`,
      });
      if (response.ok) {
        toast("标签已保存");
        profileState.reload();
      }
    });
  };

  const setBlocked = async (blocked: boolean) => {
    setBlockOpen(false);
    await run(async () => {
      const response = await dispatch(ACTIONS.relationBlock, { user_id: userId, blocked }, {
        idempotencyKey: `block:${userId}:${blocked}`,
      });
      if (response.ok) {
        toast(blocked ? "已拉黑，对方无法再发消息" : "已解除拉黑");
        profileState.reload();
      }
    });
  };

  if (profileState.loading) return <LoadingState label="正在读取关系资料…" />;
  if (profileState.error) {
    return (
      <section className="flex min-h-0 min-w-0 flex-1 items-center justify-center bg-[var(--surface)]">
        <ErrorState error={profileState.error} onRetry={profileState.reload} />
      </section>
    );
  }
  if (!profile) {
    return <section className="flex min-h-0 min-w-0 flex-1 bg-[var(--surface)]" />;
  }

  return (
    <section className="scrollbar-thin min-h-0 min-w-0 flex-1 overflow-y-auto bg-[var(--surface)]" data-testid="contact-detail">
      <div className="border-b border-[var(--line)] bg-[var(--surface-2)] px-6 py-5">
        <div className="flex items-start gap-4">
          <Avatar src={profile.avatar} name={profile.name} size={64} rounded="rounded-[12px]" online={contact?.online} />
          <div className="min-w-0 flex-1">
            <div className="flex items-center gap-2">
              <h2 className="truncate text-[17px] font-semibold text-[var(--ink)]">{profile.name}</h2>
              {contact?.star ? <Star size={13} className="text-[var(--warn)]" /> : null}
              {profile.blocked ? <span className="rounded bg-[var(--danger)]/10 px-1.5 py-0.5 text-[9px] text-[var(--danger)]">已拉黑</span> : null}
              {!profile.is_friend ? <span className="rounded bg-[var(--surface-3)] px-1.5 py-0.5 text-[9px] text-[var(--muted)]">非好友</span> : null}
            </div>
            <p className="mt-1 text-[10.5px] text-[var(--muted)]">
              {profile.region ? `${profile.region} · ` : ""}ID {profile.user_id}
            </p>
            <p className="mt-1 text-[10.5px] text-[var(--ink-2)]">{profile.signature ?? (contact?.note || "暂无签名")}</p>
          </div>
          <div className="flex shrink-0 items-center gap-1.5">
            {profile.is_friend ? (
              <>
                <Button size="sm" variant="primary" disabled={profile.blocked} onClick={() => void openChat()} data-testid="contact-message">
                  <MessageSquare size={12} /> 发消息
                </Button>
                <Button size="sm" onClick={() => void toggleStar()} pending={pending} data-testid="contact-star">
                  <Star size={12} className={contact?.star ? "text-[var(--warn)]" : undefined} /> {contact?.star ? "取消星标" : "星标"}
                </Button>
                <Button size="sm" variant="ghost" onClick={() => setBlockOpen(true)} disabled={pending} data-testid="contact-block-toggle">
                  <Ban size={12} /> {profile.blocked ? "解除拉黑" : "拉黑"}
                </Button>
              </>
            ) : (
              <Button
                size="sm"
                variant="primary"
                onClick={async () => {
                  const response = await dispatch(ACTIONS.requestSend, { user_id: userId, message: "" }, { idempotencyKey: `friend:${userId}` });
                  if (response.ok) toast("好友申请已提交");
                }}
                data-testid="contact-add-friend"
              >
                <UserPlus size={12} /> 加为好友
              </Button>
            )}
          </div>
        </div>
      </div>

      <div className="grid gap-4 px-6 py-5 lg:grid-cols-2">
        <div className="rounded-lg border border-[var(--line-strong)] bg-[var(--surface-2)] p-4">
          <h3 className="text-[11px] font-semibold text-[var(--ink)]">备注与标签</h3>
          <div className="mt-3 space-y-3">
            <div>
              <TextField
                label="备注名"
                value={note}
                onChange={(event) => setNote(event.target.value)}
                error={noteError ?? undefined}
                hint="最多 32 字；保存后写入服务端关系链"
                data-testid="contact-note"
              />
              <Button size="sm" className="mt-1.5" onClick={() => void saveNote()} pending={pending} data-testid="contact-note-save">
                保存备注
              </Button>
            </div>
            <div>
              <TextField
                label="标签"
                value={tags}
                onChange={(event) => setTags(event.target.value)}
                error={tagError ?? undefined}
                hint="用逗号分隔，最多 10 个"
                data-testid="contact-tags"
              />
              <Button size="sm" className="mt-1.5" onClick={() => void saveTags()} pending={pending} data-testid="contact-tags-save">
                保存标签
              </Button>
            </div>
          </div>
        </div>

        <div className="rounded-lg border border-[var(--line-strong)] bg-[var(--surface-2)] p-4">
          <h3 className="text-[11px] font-semibold text-[var(--ink)]">共同群聊</h3>
          {profile.common_groups.length === 0 ? (
            <p className="mt-2 text-[10px] text-[var(--muted)]">没有共同群聊。</p>
          ) : (
            <ul className="mt-2 space-y-1">
              {profile.common_groups.map((group) => (
                <li key={group.group_id}>
                  <button
                    type="button"
                    onClick={() => navigate({ id: "group", groupId: group.group_id })}
                    className="flex w-full items-center justify-between rounded px-2 py-1.5 text-left text-[10.5px] text-[var(--ink-2)] hover:bg-[var(--surface-3)]"
                    data-testid={`common-group-${group.group_id}`}
                  >
                    <span>{group.name}</span>
                    <span className="text-[9px] text-[var(--muted)]">{group.group_id}</span>
                  </button>
                </li>
              ))}
            </ul>
          )}
          <p className="mt-3 text-[9px] leading-4 text-[var(--muted)]">
            修改备注、标签、星标与拉黑都会触发核心事件；列表与搜索结果随后刷新，不以本地缓存为准。
          </p>
        </div>
      </div>

      <ConfirmDialog
        open={blockOpen}
        title={profile.blocked ? "解除拉黑？" : "拉黑该联系人？"}
        description={profile.blocked ? "对方将重新可以向你发送消息。" : "拉黑后对方消息会被服务端拦截；你可以随时解除。"}
        confirmLabel={profile.blocked ? "解除拉黑" : "拉黑"}
        danger={!profile.blocked}
        pending={pending}
        onConfirm={() => void setBlocked(!profile.blocked)}
        onClose={() => setBlockOpen(false)}
      />
    </section>
  );
}
