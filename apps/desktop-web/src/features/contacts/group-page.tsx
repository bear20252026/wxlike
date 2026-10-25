import { useEffect, useState } from "react";
import { ArrowLeft, Shield, UserMinus, Users } from "lucide-react";
import { ACTIONS } from "../../shell/core-contract";
import type { GroupMember, GroupRole } from "../../shell/models";
import { useAppState, useCoreAction, useToast } from "../../shell/store";
import { useAsync, usePending } from "../../shell/hooks";
import { navigate } from "../../shell/router";
import { Avatar, Button, ConfirmDialog, EmptyState, ErrorState, LoadingState, PermissionDenied, TextField, cn } from "../../shell/ui";

interface GroupPayload {
  group_id: string;
  name: string;
  my_role: GroupRole;
  members: GroupMember[];
}

const ROLE_LABEL: Record<GroupRole, string> = { owner: "群主", admin: "管理员", member: "成员" };

export function GroupPage({ groupId }: { groupId: string }) {
  const state = useAppState();
  const dispatch = useCoreAction();
  const toast = useToast();
  const { pending, run } = usePending();
  const group = useAsync<GroupPayload>(() => dispatch(ACTIONS.groupMembers, { group_id: groupId }), [groupId, state.searchEpoch]);
  const [name, setName] = useState("");
  const [nameError, setNameError] = useState<string | null>(null);
  const [kickTarget, setKickTarget] = useState<GroupMember | null>(null);

  useEffect(() => {
    if (group.data) setName(group.data.name);
  }, [group.data]);

  if (group.loading) return <LoadingState label="正在读取群成员…" />;
  if (group.error) {
    if (group.error.code === "FORBIDDEN") {
      return (
        <PermissionDenied
          reason={`${group.error.message}。群资料不会因为隐藏按钮而放宽服务端鉴权。`}
          action={<Button size="sm" onClick={() => navigate({ id: "chats" })}>返回会话</Button>}
        />
      );
    }
    return <ErrorState error={group.error} onRetry={group.reload} />;
  }
  if (!group.data) {
    return <EmptyState icon={<Users size={20} />} title="群不存在" description="可能已退群或群已解散。" />;
  }

  const data = group.data;
  const manages = data.my_role === "owner" || data.my_role === "admin";

  const saveName = async () => {
    if (!name.trim()) {
      setNameError("群名称不能为空");
      return;
    }
    setNameError(null);
    await run(async () => {
      const response = await dispatch(ACTIONS.groupSettings, { group_id: groupId, name: name.trim() }, {
        idempotencyKey: `group-name:${groupId}:${name.trim()}`,
      });
      if (response.ok) {
        toast("群名称已更新");
        group.reload();
      } else if (response.error.field_errors?.name) {
        setNameError(response.error.field_errors.name);
      }
    });
  };

  const kick = async () => {
    if (!kickTarget) return;
    const target = kickTarget;
    setKickTarget(null);
    await run(async () => {
      const response = await dispatch(ACTIONS.groupKick, { group_id: groupId, user_id: target.user_id }, {
        idempotencyKey: `kick:${groupId}:${target.user_id}`,
      });
      if (response.ok) {
        toast(`已将 ${target.name} 移出群聊`);
        group.reload();
      }
    });
  };

  const conv = state.conversations.find((row) => row.title.startsWith(data.name) || row.conv_id === groupId.replace(/^group-/, "conv-"));

  return (
    <div className="scrollbar-thin min-h-0 flex-1 overflow-y-auto bg-[var(--surface)]" data-testid="group-page">
      <header className="flex h-[52px] items-center gap-2 border-b border-[var(--line)] bg-[var(--surface-2)] px-4">
        <button
          type="button"
          onClick={() => navigate({ id: "chats" })}
          className="flex items-center gap-1 text-[10.5px] text-[var(--muted)] hover:text-[var(--ink)]"
          data-testid="group-back"
        >
          <ArrowLeft size={13} /> 会话
        </button>
        <h1 className="ml-2 text-[13px] font-semibold text-[var(--ink)]">{data.name}</h1>
        <span className="rounded bg-[var(--surface-3)] px-1.5 py-0.5 text-[8.5px] text-[var(--muted)]">{ROLE_LABEL[data.my_role]}</span>
        {conv ? (
          <Button size="sm" className="ml-auto" onClick={() => navigate({ id: "chat", convId: conv.conv_id })}>
            打开群聊
          </Button>
        ) : null}
      </header>

      <div className="mx-auto grid max-w-[860px] gap-4 px-5 py-5 lg:grid-cols-[280px_1fr]">
        <div className="rounded-lg border border-[var(--line-strong)] bg-[var(--surface-2)] p-4">
          <h2 className="text-[11px] font-semibold text-[var(--ink)]">群设置</h2>
          <p className="mt-1 text-[9px] leading-4 text-[var(--muted)]">{data.members.length} 位成员 · {data.group_id}</p>
          {manages ? (
            <div className="mt-3">
              <TextField
                label="群名称"
                value={name}
                onChange={(event) => setName(event.target.value)}
                error={nameError ?? undefined}
                data-testid="group-name"
              />
              <Button size="sm" className="mt-1.5" onClick={() => void saveName()} pending={pending} data-testid="group-name-save">
                保存群名称
              </Button>
            </div>
          ) : (
            <div className="mt-3 rounded-md bg-[var(--surface-3)] p-2.5">
              <p className="flex items-start gap-1.5 text-[9.5px] leading-4 text-[var(--muted)]">
                <Shield size={12} className="mt-0.5 shrink-0" />
                只有群主或管理员可以改名、移除成员。界面按角色隐藏入口，服务端仍会再次鉴权。
              </p>
            </div>
          )}
        </div>

        <div className="rounded-lg border border-[var(--line-strong)] bg-[var(--surface-2)] p-4">
          <div className="flex items-center justify-between">
            <h2 className="text-[11px] font-semibold text-[var(--ink)]">成员</h2>
            <span className="text-[9px] text-[var(--muted)]">按权限显示操作</span>
          </div>
          <ul className="mt-2 divide-y divide-[var(--line)]">
            {data.members.map((member) => (
              <li key={member.user_id} className="flex items-center gap-2 py-2" data-testid={`group-member-${member.user_id}`}>
                <Avatar src={member.avatar} name={member.name} size={30} />
                <div className="min-w-0 flex-1">
                  <div className="flex items-center gap-1.5">
                    <span className="truncate text-[11px] text-[var(--ink)]">{member.name}</span>
                    <span
                      className={cn(
                        "rounded px-1 py-0.5 text-[8px]",
                        member.role === "owner"
                          ? "bg-[var(--warn)]/12 text-[var(--warn)]"
                          : member.role === "admin"
                            ? "bg-[var(--accent-soft)] text-[var(--accent-ink)]"
                            : "bg-[var(--surface-3)] text-[var(--muted)]",
                      )}
                    >
                      {ROLE_LABEL[member.role]}
                    </span>
                  </div>
                  <span className="block text-[8.5px] text-[var(--muted)]">ID {member.user_id}</span>
                </div>
                {manages && member.role !== "owner" ? (
                  <Button size="sm" variant="ghost" onClick={() => setKickTarget(member)} data-testid={`group-kick-${member.user_id}`}>
                    <UserMinus size={11} /> 移除
                  </Button>
                ) : null}
              </li>
            ))}
          </ul>
        </div>
      </div>

      <ConfirmDialog
        open={kickTarget !== null}
        title={`将 ${kickTarget?.name ?? ""} 移出群聊？`}
        description="移除后服务端会撤回其群消息权限；此操作需要群主或管理员权限。"
        confirmLabel="移除"
        danger
        pending={pending}
        onConfirm={() => void kick()}
        onClose={() => setKickTarget(null)}
      />
    </div>
  );
}
