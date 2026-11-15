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

// 一条命令参数的规格（与 crates/cmd 的 Param 声明同源）。
data class ParamSpec(val name: String, val type: String, val required: Boolean)

// 逐命令的参数规格。
val actionParams: Map<String, List<ParamSpec>> = mapOf(
    "channel.roles" to listOf(ParamSpec("conv", "String", true)),
    "community.threads" to listOf(ParamSpec("community", "String", true), ParamSpec("limit", "Long", false)),
    "conv.list" to emptyList(),
    "conv.unread" to listOf(ParamSpec("conv", "String", true)),
    "conv.history" to listOf(ParamSpec("conv", "String", true), ParamSpec("before_seq", "Long", false), ParamSpec("limit", "Long", false)),
    "conv.sync" to listOf(ParamSpec("conv", "String", true), ParamSpec("version", "Long", true)),
    "conv.ack" to listOf(ParamSpec("conv", "String", true), ParamSpec("seq", "Long", false)),
    "friend.list" to emptyList(),
    "friend.requests.incoming" to emptyList(),
    "friend.requests.outgoing" to emptyList(),
    "friend.request.create" to listOf(ParamSpec("to_user_id", "String", true), ParamSpec("message", "String", false)),
    "friend.request.accept" to listOf(ParamSpec("req_id", "String", true)),
    "friend.request.decline" to listOf(ParamSpec("req_id", "String", true)),
    "friend.remark.set" to listOf(ParamSpec("user_id", "String", true), ParamSpec("remark", "String", true)),
    "friend.remove" to listOf(ParamSpec("user_id", "String", true), ParamSpec("confirmation_token", "String", false)),
    "friend.block.list" to emptyList(),
    "friend.block.add" to listOf(ParamSpec("user_id", "String", true), ParamSpec("confirmation_token", "String", false)),
    "friend.block.remove" to listOf(ParamSpec("user_id", "String", true)),
    "friend.privacy.get" to emptyList(),
    "friend.privacy.set" to listOf(ParamSpec("allow", "Boolean", true)),
    "group.members" to listOf(ParamSpec("conv", "String", true), ParamSpec("after_user", "String", false), ParamSpec("limit", "Long", false)),
    "group.info" to listOf(ParamSpec("conv", "String", true)),
    "group.announcement.get" to listOf(ParamSpec("conv", "String", true)),
    "group.mute_all.get" to listOf(ParamSpec("conv", "String", true)),
    "group.create" to listOf(ParamSpec("name", "String", true), ParamSpec("member_ids", "List<String>", false), ParamSpec("idempotency_key", "String", true)),
    "group.add_member" to listOf(ParamSpec("conv", "String", true), ParamSpec("user_ids", "List<String>", true)),
    "group.announcement.set" to listOf(ParamSpec("conv", "String", true), ParamSpec("text", "String", true), ParamSpec("media_id", "String", false)),
    "group.mute_all.set" to listOf(ParamSpec("conv", "String", true), ParamSpec("on", "Boolean", true)),
    "group.kick" to listOf(ParamSpec("conv", "String", true), ParamSpec("user_id", "String", true), ParamSpec("confirmation_token", "String", false)),
    "me.info" to emptyList(),
    "me.sessions" to emptyList(),
    "media.files.list" to listOf(ParamSpec("category", "String", false), ParamSpec("folder", "String", false), ParamSpec("sort", "String", false), ParamSpec("cursor", "String", false), ParamSpec("limit", "Long", false)),
    "media.upload.get" to listOf(ParamSpec("upload_id", "String", true)),
    "media.upload.start" to listOf(ParamSpec("purpose", "String", true), ParamSpec("name", "String", true), ParamSpec("size", "Long", true), ParamSpec("mime", "String", true), ParamSpec("digest", "String", false)),
    "media.upload.complete" to listOf(ParamSpec("upload_id", "String", true), ParamSpec("digest", "String", false)),
    "media.upload.cancel" to listOf(ParamSpec("upload_id", "String", true)),
    "media.object.access" to listOf(ParamSpec("object_id", "String", true)),
    "media.object.revoke" to listOf(ParamSpec("object_id", "String", true)),
    "msg.list" to listOf(ParamSpec("conv", "String", true), ParamSpec("from_seq", "Long", true), ParamSpec("to_seq", "Long", false)),
    "msg.search" to listOf(ParamSpec("q", "String", true), ParamSpec("limit", "Long", false)),
    "msg.get" to listOf(ParamSpec("conv", "String", true), ParamSpec("msg_id", "String", true)),
    "msg.send" to listOf(ParamSpec("conv", "String", true), ParamSpec("text", "String", true), ParamSpec("reply_to", "String", false), ParamSpec("idempotency_key", "String", true)),
    "msg.forward" to listOf(ParamSpec("from_conv", "String", true), ParamSpec("to_conv", "String", true), ParamSpec("msg_id", "String", true), ParamSpec("idempotency_key", "String", true)),
    "msg.recall" to listOf(ParamSpec("conv", "String", true), ParamSpec("msg_id", "String", true), ParamSpec("confirmation_token", "String", false)),
    "video.select" to listOf(ParamSpec("video_id", "String", true), ParamSpec("kbps", "Long", true)),
    "video.stats" to listOf(ParamSpec("video_id", "String", true)),
    "video.job.get" to listOf(ParamSpec("job_id", "String", true)),
    "video.job.create" to listOf(ParamSpec("path", "String", true), ParamSpec("callback_url", "String", false)),
    "video.job.retry" to listOf(ParamSpec("job_id", "String", true)),
    "video.view.report" to listOf(ParamSpec("video_id", "String", true), ParamSpec("viewer", "String", true), ParamSpec("watched_ms", "Long", true), ParamSpec("position_ms", "Long", false), ParamSpec("completed", "Boolean", false), ParamSpec("session_id", "String", false)),
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

