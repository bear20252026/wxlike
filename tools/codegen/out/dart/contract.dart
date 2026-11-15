// 由 tools/codegen/generate.py 生成，**不要手改**。
// schema: contract-v1  摘要: ad2d2be657637cf7  contract: 1.0
// 改了契约就重跑生成器；`--check` 会在 CI 上盯着这一步。


// ignore_for_file: unused_field

const String schemaVersion = 'contract-v1';
const String contractVersion = '1.0';
const String schemaDigest = 'ad2d2be657637cf7';

class Money {
  final String minor;
  final String currency;

  const Money({
    this.minor,
    this.currency,
  });

  factory Money.fromJson(Map<String, dynamic> json) {
    return Money(
      minor: json['minor'] as String?,
      currency: json['currency'] as String?,
    );
  }

  Map<String, dynamic> toJson() => {
    'minor': minor,
    'currency': currency,
  };
}

class DomainError {
  final String code;
  final String message;
  final String? field;
  final int? retry_after_ms;

  const DomainError({
    this.code,
    this.message,
    this.field,
    this.retry_after_ms,
  });

  factory DomainError.fromJson(Map<String, dynamic> json) {
    return DomainError(
      code: json['code'] as String?,
      message: json['message'] as String?,
      field: json['field'] as String?,
      retry_after_ms: json['retry_after_ms'] as int?,
    );
  }

  Map<String, dynamic> toJson() => {
    'code': code,
    'message': message,
    'field': field,
    'retry_after_ms': retry_after_ms,
  };
}

class ActionRequest {
  final String action_id;
  final String? entry_id;
  final Map<String, dynamic> params;
  final String? idempotency_key;
  final String? deadline;
  final String trace_id;
  final String contract;

  const ActionRequest({
    this.action_id,
    this.entry_id,
    this.params,
    this.idempotency_key,
    this.deadline,
    this.trace_id,
    this.contract,
  });

  factory ActionRequest.fromJson(Map<String, dynamic> json) {
    return ActionRequest(
      action_id: json['action_id'] as String?,
      entry_id: json['entry_id'] as String?,
      params: json['params'] as Map<String, dynamic>?,
      idempotency_key: json['idempotency_key'] as String?,
      deadline: json['deadline'] as String?,
      trace_id: json['trace_id'] as String?,
      contract: json['contract'] as String?,
    );
  }

  Map<String, dynamic> toJson() => {
    'action_id': action_id,
    'entry_id': entry_id,
    'params': params,
    'idempotency_key': idempotency_key,
    'deadline': deadline,
    'trace_id': trace_id,
    'contract': contract,
  };
}

class ActionResult {
  final String action_id;
  final String status;
  final Map<String, dynamic>? data;
  final Map<String, dynamic>? error;
  final String? cursor;
  final int completed_at_ms;

  const ActionResult({
    this.action_id,
    this.status,
    this.data,
    this.error,
    this.cursor,
    this.completed_at_ms,
  });

  factory ActionResult.fromJson(Map<String, dynamic> json) {
    return ActionResult(
      action_id: json['action_id'] as String?,
      status: json['status'] as String?,
      data: json['data'] as Map<String, dynamic>?,
      error: json['error'] as Map<String, dynamic>?,
      cursor: json['cursor'] as String?,
      completed_at_ms: json['completed_at_ms'] as int?,
    );
  }

  Map<String, dynamic> toJson() => {
    'action_id': action_id,
    'status': status,
    'data': data,
    'error': error,
    'cursor': cursor,
    'completed_at_ms': completed_at_ms,
  };
}

class EventEnvelope {
  final String event_id;
  final String seq;
  final String? conv;
  final String kind;
  final Map<String, dynamic> payload;
  final int emitted_at_ms;
  final String contract;

  const EventEnvelope({
    this.event_id,
    this.seq,
    this.conv,
    this.kind,
    this.payload,
    this.emitted_at_ms,
    this.contract,
  });

  factory EventEnvelope.fromJson(Map<String, dynamic> json) {
    return EventEnvelope(
      event_id: json['event_id'] as String?,
      seq: json['seq'] as String?,
      conv: json['conv'] as String?,
      kind: json['kind'] as String?,
      payload: json['payload'] as Map<String, dynamic>?,
      emitted_at_ms: json['emitted_at_ms'] as int?,
      contract: json['contract'] as String?,
    );
  }

  Map<String, dynamic> toJson() => {
    'event_id': event_id,
    'seq': seq,
    'conv': conv,
    'kind': kind,
    'payload': payload,
    'emitted_at_ms': emitted_at_ms,
    'contract': contract,
  };
}

/// 已登记的命令（与 crates/cmd 的目录同源）。
const Map<String, String> actionCapabilities = {
  'channel.roles': 'read',
  'community.threads': 'read',
  'conv.list': 'read',
  'conv.unread': 'read',
  'conv.history': 'read',
  'conv.sync': 'read',
  'conv.ack': 'write',
  'friend.list': 'read',
  'friend.requests.incoming': 'read',
  'friend.requests.outgoing': 'read',
  'friend.request.create': 'write',
  'friend.request.accept': 'write',
  'friend.request.decline': 'write',
  'friend.remark.set': 'write',
  'friend.remove': 'destructive',
  'friend.block.list': 'read',
  'friend.block.add': 'destructive',
  'friend.block.remove': 'write',
  'friend.privacy.get': 'read',
  'friend.privacy.set': 'write',
  'group.members': 'read',
  'group.info': 'read',
  'group.announcement.get': 'read',
  'group.mute_all.get': 'read',
  'group.create': 'write',
  'group.add_member': 'write',
  'group.announcement.set': 'write',
  'group.mute_all.set': 'write',
  'group.kick': 'destructive',
  'me.info': 'read',
  'me.sessions': 'read',
  'media.files.list': 'read',
  'media.upload.get': 'read',
  'media.upload.start': 'write',
  'media.upload.complete': 'write',
  'media.upload.cancel': 'write',
  'media.object.access': 'write',
  'media.object.revoke': 'write',
  'msg.list': 'read',
  'msg.search': 'read',
  'msg.get': 'read',
  'msg.send': 'write',
  'msg.forward': 'write',
  'msg.recall': 'destructive',
  'video.select': 'read',
  'video.stats': 'read',
  'video.job.get': 'read',
  'video.job.create': 'write',
  'video.job.retry': 'write',
  'video.view.report': 'write',
};

/// 一条命令参数的规格（与 crates/cmd 的 Param 声明同源）。
class ParamSpec {
  final String name;
  final String type;
  final bool required;
  const ParamSpec(this.name, this.type, this.required);
}

/// 逐命令的参数规格。
const Map<String, List<ParamSpec>> actionParams = {
  'channel.roles': [ParamSpec('conv', 'String', true)],
  'community.threads': [ParamSpec('community', 'String', true), ParamSpec('limit', 'int', false)],
  'conv.list': [],
  'conv.unread': [ParamSpec('conv', 'String', true)],
  'conv.history': [ParamSpec('conv', 'String', true), ParamSpec('before_seq', 'int', false), ParamSpec('limit', 'int', false)],
  'conv.sync': [ParamSpec('conv', 'String', true), ParamSpec('version', 'int', true)],
  'conv.ack': [ParamSpec('conv', 'String', true), ParamSpec('seq', 'int', false)],
  'friend.list': [],
  'friend.requests.incoming': [],
  'friend.requests.outgoing': [],
  'friend.request.create': [ParamSpec('to_user_id', 'String', true), ParamSpec('message', 'String', false)],
  'friend.request.accept': [ParamSpec('req_id', 'String', true)],
  'friend.request.decline': [ParamSpec('req_id', 'String', true)],
  'friend.remark.set': [ParamSpec('user_id', 'String', true), ParamSpec('remark', 'String', true)],
  'friend.remove': [ParamSpec('user_id', 'String', true), ParamSpec('confirmation_token', 'String', false)],
  'friend.block.list': [],
  'friend.block.add': [ParamSpec('user_id', 'String', true), ParamSpec('confirmation_token', 'String', false)],
  'friend.block.remove': [ParamSpec('user_id', 'String', true)],
  'friend.privacy.get': [],
  'friend.privacy.set': [ParamSpec('allow', 'bool', true)],
  'group.members': [ParamSpec('conv', 'String', true), ParamSpec('after_user', 'String', false), ParamSpec('limit', 'int', false)],
  'group.info': [ParamSpec('conv', 'String', true)],
  'group.announcement.get': [ParamSpec('conv', 'String', true)],
  'group.mute_all.get': [ParamSpec('conv', 'String', true)],
  'group.create': [ParamSpec('name', 'String', true), ParamSpec('member_ids', 'List<String>', false), ParamSpec('idempotency_key', 'String', true)],
  'group.add_member': [ParamSpec('conv', 'String', true), ParamSpec('user_ids', 'List<String>', true)],
  'group.announcement.set': [ParamSpec('conv', 'String', true), ParamSpec('text', 'String', true), ParamSpec('media_id', 'String', false)],
  'group.mute_all.set': [ParamSpec('conv', 'String', true), ParamSpec('on', 'bool', true)],
  'group.kick': [ParamSpec('conv', 'String', true), ParamSpec('user_id', 'String', true), ParamSpec('confirmation_token', 'String', false)],
  'me.info': [],
  'me.sessions': [],
  'media.files.list': [ParamSpec('category', 'String', false), ParamSpec('folder', 'String', false), ParamSpec('sort', 'String', false), ParamSpec('cursor', 'String', false), ParamSpec('limit', 'int', false)],
  'media.upload.get': [ParamSpec('upload_id', 'String', true)],
  'media.upload.start': [ParamSpec('purpose', 'String', true), ParamSpec('name', 'String', true), ParamSpec('size', 'int', true), ParamSpec('mime', 'String', true), ParamSpec('digest', 'String', false)],
  'media.upload.complete': [ParamSpec('upload_id', 'String', true), ParamSpec('digest', 'String', false)],
  'media.upload.cancel': [ParamSpec('upload_id', 'String', true)],
  'media.object.access': [ParamSpec('object_id', 'String', true)],
  'media.object.revoke': [ParamSpec('object_id', 'String', true)],
  'msg.list': [ParamSpec('conv', 'String', true), ParamSpec('from_seq', 'int', true), ParamSpec('to_seq', 'int', false)],
  'msg.search': [ParamSpec('q', 'String', true), ParamSpec('limit', 'int', false)],
  'msg.get': [ParamSpec('conv', 'String', true), ParamSpec('msg_id', 'String', true)],
  'msg.send': [ParamSpec('conv', 'String', true), ParamSpec('text', 'String', true), ParamSpec('reply_to', 'String', false), ParamSpec('idempotency_key', 'String', true)],
  'msg.forward': [ParamSpec('from_conv', 'String', true), ParamSpec('to_conv', 'String', true), ParamSpec('msg_id', 'String', true), ParamSpec('idempotency_key', 'String', true)],
  'msg.recall': [ParamSpec('conv', 'String', true), ParamSpec('msg_id', 'String', true), ParamSpec('confirmation_token', 'String', false)],
  'video.select': [ParamSpec('video_id', 'String', true), ParamSpec('kbps', 'int', true)],
  'video.stats': [ParamSpec('video_id', 'String', true)],
  'video.job.get': [ParamSpec('job_id', 'String', true)],
  'video.job.create': [ParamSpec('path', 'String', true), ParamSpec('callback_url', 'String', false)],
  'video.job.retry': [ParamSpec('job_id', 'String', true)],
  'video.view.report': [ParamSpec('video_id', 'String', true), ParamSpec('viewer', 'String', true), ParamSpec('watched_ms', 'int', true), ParamSpec('position_ms', 'int', false), ParamSpec('completed', 'bool', false), ParamSpec('session_id', 'String', false)],
};

/// 解码一条命令结果；未知 status 返回 null（跳过，不抛）。
ActionResult? decodeActionResult(Object? raw) {
  if (raw is! Map<String, dynamic>) return null;
  final status = raw['status'];
  if (status != 'ok' && status != 'empty' && status != 'failed') return null;
  if (raw['action_id'] is! String) return null;
  return ActionResult.fromJson(raw);
}

/// 金额：字符串 → BigInt（Dart web 上 int 是 double，必须走 BigInt）。
BigInt moneyToMinor(Money money) => BigInt.parse(money.minor!);

/// 这条命令认不认识。
bool isKnownAction(String actionId) => actionCapabilities.containsKey(actionId);

