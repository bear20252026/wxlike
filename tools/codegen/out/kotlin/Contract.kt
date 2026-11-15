// 由 tools/codegen/generate.py 生成，**不要手改**。
// schema: contract-v1  摘要: ad2d2be657637cf7  contract: 1.0
// 改了契约就重跑生成器；`--check` 会在 CI 上盯着这一步。


@file:Suppress("unused")

package wxlike.contract

const val SCHEMA_VERSION = "contract-v1"
const val CONTRACT_VERSION = "1.0"
const val SCHEMA_DIGEST = "ad2d2be657637cf7"

data class Money(
    val minor: String,
    val currency: String
)

data class DomainError(
    val code: String,
    val message: String,
    val field: String? = null,
    val retry_after_ms: Long? = null
)

data class ActionRequest(
    val action_id: String,
    val entry_id: String? = null,
    val params: Map<String, Any>,
    val idempotency_key: String? = null,
    val deadline: String? = null,
    val trace_id: String,
    val contract: String
)

data class ActionResult(
    val action_id: String,
    val status: String,
    val data: Map<String, Any>? = null,
    val error: Map<String, Any>? = null,
    val cursor: String? = null,
    val completed_at_ms: Long
)

data class EventEnvelope(
    val event_id: String,
    val seq: String,
    val conv: String? = null,
    val kind: String,
    val payload: Map<String, Any>,
    val emitted_at_ms: Long,
    val contract: String
)

// 已登记的命令（与 crates/cmd 的目录同源）。
val actionCapabilities: Map<String, String> = mapOf(
    "channel.roles" to "read",
    "community.threads" to "read",
    "conv.list" to "read",
    "conv.unread" to "read",
    "conv.history" to "read",
    "conv.sync" to "read",
    "conv.ack" to "write",
    "friend.list" to "read",
    "friend.requests.incoming" to "read",
    "friend.requests.outgoing" to "read",
    "friend.request.create" to "write",
    "friend.request.accept" to "write",
    "friend.request.decline" to "write",
    "friend.remark.set" to "write",
    "friend.remove" to "destructive",
    "friend.block.list" to "read",
    "friend.block.add" to "destructive",
    "friend.block.remove" to "write",
    "friend.privacy.get" to "read",
    "friend.privacy.set" to "write",
    "group.members" to "read",
    "group.info" to "read",
    "group.announcement.get" to "read",
    "group.mute_all.get" to "read",
    "group.create" to "write",
    "group.add_member" to "write",
    "group.announcement.set" to "write",
    "group.mute_all.set" to "write",
    "group.kick" to "destructive",
    "me.info" to "read",
    "me.sessions" to "read",
    "media.files.list" to "read",
    "media.upload.get" to "read",
    "media.upload.start" to "write",
    "media.upload.complete" to "write",
    "media.upload.cancel" to "write",
    "media.object.access" to "write",
    "media.object.revoke" to "write",
    "msg.list" to "read",
    "msg.search" to "read",
    "msg.get" to "read",
    "msg.send" to "write",
    "msg.forward" to "write",
    "msg.recall" to "destructive",
    "video.select" to "read",
    "video.stats" to "read",
    "video.job.get" to "read",
    "video.job.create" to "write",
    "video.job.retry" to "write",
    "video.view.report" to "write",
)

// 解码一条命令结果；未知 status 返回 null（跳过，不抛）。
//
// 服务端先上线新状态、旧客户端后升级是常态：这时候应当"跳过并记一笔"，
// 不是抛异常把整条渲染链打断。与 TS/Dart 的 decodeActionResult 同语义。
fun decodeActionResult(raw: Any?): ActionResult? {
    val value = raw as? Map<*, *> ?: return null
    val actionId = value["action_id"] as? String ?: return null
    val status = value["status"] as? String ?: return null
    if (status != "ok" && status != "empty" && status != "failed") return null
    val completedAtMs = (value["completed_at_ms"] as? Number)?.toLong() ?: return null
    return ActionResult(
        action_id = actionId,
        status = status,
        data = value["data"] as? Map<String, Any>,
        error = value["error"] as? Map<String, Any>,
        cursor = value["cursor"] as? String,
        completed_at_ms = completedAtMs,
    )
}

// 这条命令认不认识。
fun isKnownAction(actionId: String): Boolean = actionCapabilities.containsKey(actionId)

