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
  'conv.list': 'read',
  'conv.unread': 'read',
  'conv.history': 'read',
  'conv.sync': 'read',
  'msg.list': 'read',
  'msg.search': 'read',
  'msg.get': 'read',
  'group.members': 'read',
  'group.info': 'read',
  'community.threads': 'read',
  'channel.roles': 'read',
  'me.info': 'read',
  'me.sessions': 'read',
  'msg.send': 'write',
  'conv.ack': 'write',
  'msg.forward': 'write',
  'group.create': 'write',
  'group.add_member': 'write',
  'video.select': 'read',
  'video.stats': 'read',
  'video.job.get': 'read',
  'video.job.create': 'write',
  'video.job.retry': 'write',
  'video.view.report': 'write',
  'group.kick': 'destructive',
  'msg.recall': 'destructive',
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

