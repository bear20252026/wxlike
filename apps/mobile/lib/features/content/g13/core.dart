/// G13 的公共契约层。
///
/// 这一层只做三件事：把服务端契约的形状固定下来（`PageResult` / `Money` /
/// `DomainFailure`），把"界面能问的问题"收敛成 `G13Ports`，以及给所有扩展页
/// 提供同一套加载/空/失败/重试行为。
///
/// 业务判断不在这里：金额怎么算、红包怎么抢、状态何时过期都在 Rust 与各自的
/// 域服务里。界面拿到的必须是"可以直接渲染的结果"，而不是需要再算一遍的原始
/// 数据。端口未接线时抛 [`G13Failure.upstreamNotWired`]，页面显示明确的阻塞
/// 状态，不用静态样例冒充成功。
library;

import 'dart:math';

/// 契约版本。与服务端 `DomainError`/游标语义一起升级，不能只改一侧。
const String g13ContractVersion = 'g13/1';

/// 一次查询的分页结果。字段名与 `01_contracts_and_data.md` 的
/// `items,next_cursor,has_more,snapshot_version` 一一对应。
class PageResult<T> {
  const PageResult({
    required this.items,
    required this.hasMore,
    required this.snapshotVersion,
    this.nextCursor = '',
  });

  final List<T> items;
  final bool hasMore;
  final String nextCursor;

  /// 服务端快照版本：界面据此判断"这页是不是比手上的新"，
  /// 不能用本地时间戳代替。
  final int snapshotVersion;

  bool get isEmpty => items.isEmpty;

  static PageResult<T> empty<T>() =>
      PageResult<T>(items: const <Never>[], hasMore: false, snapshotVersion: 0);
}

/// 金额一律是"最小单位 + 币种"，与 `commerce.*` 契约一致。
///
/// 界面**不允许**把 `minor` 除以 100 自己拼字符串：显示必须经过
/// `G13Ports.formatMoney`（绑定的 Rust 实现负责精确格式化、负数与币种符号）。
class Money {
  const Money({required this.minor, required this.currency});

  final int minor;
  final String currency;

  bool get isZero => minor == 0;

  @override
  bool operator ==(Object other) =>
      other is Money && other.minor == minor && other.currency == currency;

  @override
  int get hashCode => Object.hash(minor, currency);

  @override
  String toString() => 'Money($minor $currency)';
}

/// 失败类别。界面只按类别决定文案与是否给"重试"，具体原因由端口层判定。
enum G13FailureKind {
  offline,
  timeout,
  unauthorized,
  forbidden,
  invalidArgument,
  conflict,
  unavailable,
  unexpected,
}

/// 领域失败。所有端口只允许抛这一种异常，界面因此不需要认识传输层。
class G13Failure implements Exception {
  const G13Failure(
    this.kind,
    this.code,
    this.message, {
    this.retryable = false,
    this.operationId = '',
  });

  final G13FailureKind kind;

  /// 公共 `DomainError` 的机器可读码，日志与测试用它，不拿文案做判断。
  final String code;
  final String message;
  final bool retryable;

  /// 长操作的 operation_id；没有则为空串。查询权限不能只凭它。
  final String operationId;

  /// 上游绑定尚未生成时唯一允许的失败：它不是"空结果"，也不是"成功"。
  factory G13Failure.upstreamNotWired(String port, [String? detail]) =>
      G13Failure(
        G13FailureKind.unavailable,
        'UPSTREAM_NOT_WIRED',
        '${detail ?? port} 尚未接入生成绑定',
      );

  bool get isUnauthorized => kind == G13FailureKind.unauthorized;

  /// 给用户看的一句短话。重试按钮只在 `retryable` 时出现。
  String get displayText => switch (kind) {
    G13FailureKind.offline => '网络已断开，请检查连接',
    G13FailureKind.timeout => '请求超时，可以重试',
    G13FailureKind.unauthorized => '登录已失效，请重新登录',
    G13FailureKind.forbidden => message.isEmpty ? '没有权限执行这个操作' : message,
    G13FailureKind.invalidArgument => message,
    G13FailureKind.conflict => '数据已变化，请刷新后再试',
    G13FailureKind.unavailable => message,
    G13FailureKind.unexpected => '出错了：$message',
  };

  @override
  String toString() => 'G13Failure($code): $message';
}

/// 危险动作的确认票据。
///
/// 确认令牌必须绑定规范化参数摘要：先确认 10 元、再换成 1000 元是明确禁止的。
/// 界面只负责把票据原样回传给 `execute`，不在本地拼参数。
class ActionConfirmation {
  const ActionConfirmation({
    required this.confirmToken,
    required this.summary,
    required this.expiresAt,
  });

  final String confirmToken;

  /// 服务端算好的、给人看的参数摘要（金额、收款方、退改条件）。
  final String summary;
  final DateTime expiresAt;
}

/// 长操作句柄（支付、识别、转码、备份都用同一形状）。
class OperationHandle {
  const OperationHandle({
    required this.operationId,
    required this.state,
    required this.progress,
    required this.canCancel,
  });

  final String operationId;

  /// `PENDING` / `RUNNING` / `SUCCEEDED` / `FAILED` / `CANCELLED` /
  /// `PENDING_RECONCILIATION`。未知值按"未完成"处理，不能当成功。
  final String state;
  final double progress;
  final bool canCancel;

  bool get isTerminal =>
      state == 'SUCCEEDED' || state == 'FAILED' || state == 'CANCELLED';
  bool get isSucceeded => state == 'SUCCEEDED';
}

/// 媒体引用：只存对象 id / 短时地址，不把本地路径当永久地址。
class MediaRef {
  const MediaRef({
    required this.mediaId,
    this.thumbnailUrl = '',
    this.durationMs = 0,
  });

  final String mediaId;
  final String thumbnailUrl;
  final int durationMs;
}

/// 幂等键：同一个"用户意图"在重试时必须复用同一个键。
///
/// 每次点击生成一个新键就是把重复提交变成两笔业务，因此键在页面状态里只生成
/// 一次，随重试与"确定"一起复用。
String newIdempotencyKey() {
  final random = Random();
  final stamp = DateTime.now().microsecondsSinceEpoch;
  return '$stamp-${random.nextInt(1 << 32).toRadixString(16)}';
}

/// 把键盘输入的"元"文本转成最小单位整数。
///
/// 全程整数运算：`double.parse` 在 0.1 + 0.2 这类输入上会引入精度误差，
/// 金额一旦舍入就是账实不符。非法输入（多个小数点、超过两位小数、负数）
/// 返回 null，由调用方禁用提交。
int? parseMinorAmount(String text) {
  if (text.isEmpty || text == '.') return null;
  final parts = text.split('.');
  if (parts.length > 2) return null;
  final whole = parts.first.isEmpty ? '0' : parts.first;
  final fraction = parts.length == 2 ? parts[1] : '';
  if (fraction.length > 2) return null;
  if (whole.length > 9) return null;
  if (!RegExp(r'^\d+$').hasMatch(whole)) return null;
  if (fraction.isNotEmpty && !RegExp(r'^\d+$').hasMatch(fraction)) return null;
  final wholeMinor = int.parse(whole) * 100;
  final fractionMinor = fraction.isEmpty
      ? 0
      : int.parse(fraction.padRight(2, '0'));
  return wholeMinor + fractionMinor;
}
