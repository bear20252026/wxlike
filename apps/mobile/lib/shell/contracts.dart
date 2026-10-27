/// G12 的公共契约层：失败分类、游标分页、时钟与幂等键。
///
/// 业务判断不在这里。界面只认识 [G12FailureKind]，具体是离线、越权还是
/// 上游未接线，由端口实现判定；页面不解析 HTTP 或异常字符串。
library;

import 'dart:math';

enum G12FailureKind {
  offline,
  timeout,
  unauthorized,
  forbidden,
  invalidArgument,
  conflict,
  unavailable,
  unexpected,
}

class G12Failure implements Exception {
  const G12Failure(
    this.kind,
    this.code,
    this.message, {
    this.retryable = false,
    this.operationId = '',
  });

  final G12FailureKind kind;
  final String code;
  final String message;
  final bool retryable;
  final String operationId;

  factory G12Failure.upstreamNotWired(String port, [String? detail]) => G12Failure(
        G12FailureKind.unavailable,
        'UPSTREAM_NOT_WIRED',
        '${detail ?? port} 尚未接入生成绑定',
      );

  bool get isUnauthorized => kind == G12FailureKind.unauthorized;

  String get displayText => switch (kind) {
        G12FailureKind.offline => '网络已断开，请检查连接',
        G12FailureKind.timeout => '请求超时，可以重试',
        G12FailureKind.unauthorized => '登录已失效，请重新登录',
        G12FailureKind.forbidden => '没有权限执行这个操作',
        G12FailureKind.invalidArgument => message,
        G12FailureKind.conflict => '数据已变化，请刷新后再试',
        G12FailureKind.unavailable => message,
        G12FailureKind.unexpected => '出错了：$message',
      };

  @override
  String toString() => 'G12Failure($code): $message';
}

/// 扫码确认页的目标：票号 + 可选的一次性 secret。
///
/// [secret] 只允许转交给 `/scan` 的请求体；[toString] 刻意不带它，
/// 这样任何日志/调试输出打印这个对象都不会泄露凭据。
class QrApprovalTarget {
  const QrApprovalTarget({required this.challengeId, this.secret = ''});

  final String challengeId;
  final String secret;

  bool get canScan => secret.trim().isNotEmpty;

  @override
  String toString() => 'QrApprovalTarget(challengeId: $challengeId, secret: <redacted>)';
}

class PageResult<T> {  const PageResult({
    required this.items,
    required this.hasMore,
    this.nextCursor = '',
    this.snapshotVersion = 0,
  });

  final List<T> items;
  final bool hasMore;
  final String nextCursor;
  final int snapshotVersion;

  bool get isEmpty => items.isEmpty;

  static PageResult<T> empty<T>() => PageResult<T>(items: const [], hasMore: false);
}

/// 单调时钟。倒计时只用它推进，避免用户改系统时间绕过重发限制。
abstract class MonotonicClock {
  Duration elapsed();
}

class StopwatchClock implements MonotonicClock {
  final Stopwatch _watch = Stopwatch()..start();

  @override
  Duration elapsed() => _watch.elapsed;
}

/// 幂等键：同一次用户动作重试必须复用同一个键。
String newIdempotencyKey([String prefix = 'g12']) {
  final micros = DateTime.now().microsecondsSinceEpoch;
  final salt = Random.secure().nextInt(0x7fffffff).toRadixString(16);
  return '$prefix-$micros-$salt';
}

/// 把异常统一成 [G12Failure]；端口实现只允许抛这一种。
G12Failure asG12Failure(Object error, {String? operation}) {
  if (error is G12Failure) return error;
  return G12Failure(
    G12FailureKind.unexpected,
    'UNEXPECTED',
    operation == null ? '$error' : '$operation 失败：$error',
  );
}

String describeG12Error(Object error) => asG12Failure(error).displayText;
