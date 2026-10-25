import { useEffect, useState } from "react";
import { Check } from "lucide-react";
import { ACTIONS } from "../../shell/core-contract";
import { useAppState, useCoreAction, useToast } from "../../shell/store";
import { Avatar, Button, Dialog, EmptyState, cn } from "../../shell/ui";
import { usePending } from "../../shell/hooks";

export function ForwardDialog({
  open,
  msgIds,
  onClose,
}: {
  open: boolean;
  msgIds: string[];
  onClose: () => void;
}) {
  const state = useAppState();
  const dispatch = useCoreAction();
  const toast = useToast();
  const { pending, run } = usePending();
  const [targets, setTargets] = useState<string[]>([]);

  useEffect(() => {
    if (open) setTargets([]);
  }, [open]);

  const toggle = (convId: string) => {
    setTargets((prev) => (prev.includes(convId) ? prev.filter((id) => id !== convId) : [...prev, convId]));
  };

  const submit = async () => {
    if (!targets.length || !msgIds.length) return;
    await run(async () => {
      const response = await dispatch(ACTIONS.messageForward, { msg_ids: msgIds, target_conv_ids: targets }, {
        idempotencyKey: `forward:${msgIds.join(",")}:${targets.join(",")}`,
      });
      if (response.ok) {
        toast(`已转发 ${msgIds.length} 条消息到 ${targets.length} 个会话`);
        onClose();
      }
    });
  };

  return (
    <Dialog
      open={open}
      title={`转发 ${msgIds.length} 条消息`}
      description="目标会话来自核心返回的会话列表；转发动作由服务端鉴权。"
      onClose={onClose}
      width={420}
      footer={
        <>
          <Button size="sm" onClick={onClose} disabled={pending}>
            取消
          </Button>
          <Button size="sm" variant="primary" pending={pending} disabled={!targets.length} onClick={() => void submit()} data-testid="forward-confirm">
            转发到 {targets.length} 个会话
          </Button>
        </>
      }
    >
      <div className="max-h-[280px] overflow-y-auto scrollbar-thin" data-testid="forward-targets">
        {state.conversations.length === 0 ? (
          <EmptyState title="没有可用会话" description="先创建或等待一个会话。" />
        ) : (
          state.conversations.map((conv) => {
            const checked = targets.includes(conv.conv_id);
            return (
              <button
                key={conv.conv_id}
                type="button"
                onClick={() => toggle(conv.conv_id)}
                className={cn(
                  "flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left",
                  checked ? "bg-[var(--accent-soft)]" : "hover:bg-[var(--surface-3)]",
                )}
                data-testid={`forward-target-${conv.conv_id}`}
              >
                <Avatar src={conv.avatar} name={conv.title} size={26} />
                <span className="min-w-0 flex-1">
                  <span className="block truncate text-[11px] text-[var(--ink)]">{conv.title}</span>
                  <span className="block truncate text-[9px] text-[var(--muted)]">{conv.last_preview || conv.kind}</span>
                </span>
                <span className={cn("grid h-4 w-4 place-items-center rounded border", checked ? "border-[var(--accent)] bg-[var(--accent)] text-white" : "border-[var(--line-strong)]")}>
                  {checked ? <Check size={10} /> : null}
                </span>
              </button>
            );
          })
        )}
      </div>
    </Dialog>
  );
}
