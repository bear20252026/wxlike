import { MessageCircle } from "lucide-react";
import type { Conversation } from "../../shell/models";
import { useAppState } from "../../shell/store";
import { navigate } from "../../shell/router";
import { EmptyState } from "../../shell/ui";
import { ConversationList } from "./conversation-list";
import { ChatView } from "./chat-view";

export function ChatsPage({ convId }: { convId?: string }) {
  const state = useAppState();
  const conv = convId ? state.conversations.find((row) => row.conv_id === convId) ?? null : null;

  const select = (next: Conversation) => navigate({ id: "chat", convId: next.conv_id });

  return (
    <div className="flex min-h-0 flex-1">
      <ConversationList activeConvId={conv?.conv_id} onSelect={select} />
      {conv ? (
        <ChatView key={conv.conv_id} conv={conv} />
      ) : (
        <section className="flex min-h-0 min-w-0 flex-1 items-center justify-center bg-[var(--surface)]">
          <EmptyState
            icon={<MessageCircle size={20} />}
            title={convId ? "会话不存在或已失效" : "选择一个会话"}
            description={
              convId
                ? "核心返回的会话列表里没有这个 ID；可能是已退群或权限已变更。"
                : "消息、草稿、未读与发送状态都由 Rust 核心投影；界面不会先伪造一份本地真相。"
            }
          />
        </section>
      )}
    </div>
  );
}
