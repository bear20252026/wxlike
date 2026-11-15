// 由 tools/codegen/generate.py 生成，**不要手改**。
// schema: contract-v1  摘要: ad2d2be657637cf7  contract: 1.0
// 改了契约就重跑生成器；`--check` 会在 CI 上盯着这一步。


import Foundation

public let schemaVersion = "contract-v1"
public let contractVersion = "1.0"
public let schemaDigest = "ad2d2be657637cf7"

public struct Money {
    public let minor: String
    public let currency: String

    public init(minor: String, currency: String) {
        self.minor = minor
        self.currency = currency
    }
}

public struct DomainError {
    public let code: String
    public let message: String
    public let field: String?
    public let retry_after_ms: Int?

    public init(code: String, message: String, field: String?, retry_after_ms: Int?) {
        self.code = code
        self.message = message
        self.field = field
        self.retry_after_ms = retry_after_ms
    }
}

public struct ActionRequest {
    public let action_id: String
    public let entry_id: String?
    public let params: [String: Any]
    public let idempotency_key: String?
    public let deadline: String?
    public let trace_id: String
    public let contract: String

    public init(action_id: String, entry_id: String?, params: [String: Any], idempotency_key: String?, deadline: String?, trace_id: String, contract: String) {
        self.action_id = action_id
        self.entry_id = entry_id
        self.params = params
        self.idempotency_key = idempotency_key
        self.deadline = deadline
        self.trace_id = trace_id
        self.contract = contract
    }
}

public struct ActionResult {
    public let action_id: String
    public let status: String
    public let data: [String: Any]?
    public let error: [String: Any]?
    public let cursor: String?
    public let completed_at_ms: Int

    public init(action_id: String, status: String, data: [String: Any]?, error: [String: Any]?, cursor: String?, completed_at_ms: Int) {
        self.action_id = action_id
        self.status = status
        self.data = data
        self.error = error
        self.cursor = cursor
        self.completed_at_ms = completed_at_ms
    }
}

public struct EventEnvelope {
    public let event_id: String
    public let seq: String
    public let conv: String?
    public let kind: String
    public let payload: [String: Any]
    public let emitted_at_ms: Int
    public let contract: String

    public init(event_id: String, seq: String, conv: String?, kind: String, payload: [String: Any], emitted_at_ms: Int, contract: String) {
        self.event_id = event_id
        self.seq = seq
        self.conv = conv
        self.kind = kind
        self.payload = payload
        self.emitted_at_ms = emitted_at_ms
        self.contract = contract
    }
}

/// 已登记的命令（与 crates/cmd 的目录同源）。
public let actionCapabilities: [String: String] = [
    "channel.roles": "read",
    "community.threads": "read",
    "conv.list": "read",
    "conv.unread": "read",
    "conv.history": "read",
    "conv.sync": "read",
    "conv.ack": "write",
    "friend.list": "read",
    "friend.requests.incoming": "read",
    "friend.requests.outgoing": "read",
    "friend.request.create": "write",
    "friend.request.accept": "write",
    "friend.request.decline": "write",
    "friend.remark.set": "write",
    "friend.remove": "destructive",
    "friend.block.list": "read",
    "friend.block.add": "destructive",
    "friend.block.remove": "write",
    "friend.privacy.get": "read",
    "friend.privacy.set": "write",
    "group.members": "read",
    "group.info": "read",
    "group.announcement.get": "read",
    "group.mute_all.get": "read",
    "group.create": "write",
    "group.add_member": "write",
    "group.announcement.set": "write",
    "group.mute_all.set": "write",
    "group.kick": "destructive",
    "me.info": "read",
    "me.sessions": "read",
    "media.files.list": "read",
    "media.upload.get": "read",
    "media.upload.start": "write",
    "media.upload.complete": "write",
    "media.upload.cancel": "write",
    "media.object.access": "write",
    "media.object.revoke": "write",
    "msg.list": "read",
    "msg.search": "read",
    "msg.get": "read",
    "msg.send": "write",
    "msg.forward": "write",
    "msg.recall": "destructive",
    "video.select": "read",
    "video.stats": "read",
    "video.job.get": "read",
    "video.job.create": "write",
    "video.job.retry": "write",
    "video.view.report": "write",
]

/// 解码一条命令结果；未知 status 返回 nil（跳过，不抛）。
///
/// 服务端先上线新状态、旧客户端后升级是常态：这时候应当"跳过并记一笔"，
/// 不是抛异常把整条渲染链打断。与 TS/Dart 的 decodeActionResult 同语义。
public func decodeActionResult(_ raw: Any?) -> ActionResult? {
    guard let value = raw as? [String: Any] else { return nil }
    guard let actionId = value["action_id"] as? String else { return nil }
    guard let status = value["status"] as? String,
          status == "ok" || status == "empty" || status == "failed" else { return nil }
    guard let completedAtMs = value["completed_at_ms"] as? Int else { return nil }
    return ActionResult(
        action_id: actionId,
        status: status,
        data: value["data"] as? [String: Any],
        error: value["error"] as? [String: Any],
        cursor: value["cursor"] as? String,
        completed_at_ms: completedAtMs
    )
}

/// 这条命令认不认识。
public func isKnownAction(_ actionId: String) -> Bool {
    return actionCapabilities[actionId] != nil
}

