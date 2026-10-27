/// 桌面登录二维码载荷 `wxlike-qr://{ticket_id}/{secret}` 的解析。
///
/// 这串字符来自摄像头或系统深链，是**外部输入**，两件事必须在这一层定死：
///
/// 1. **形状不合就不放行**。scheme 不对、缺 secret、路径段多出、带 query/fragment
///    的一律抛 [G12Failure]，绝不"猜"票 id 或补一个空 secret——猜错等于把批准
///    点到了另一张票上，服务端 `secret_must_match` 也是同一个口径（宁可 400 也不猜）。
/// 2. **secret 不落进任何可读串**。它是这张票的唯一凭据：拿到它就能 `/scan`、
///    能在 `/approve` 之后 `/exchange` 走创建者的真实会话。所以
///    [QrScanPayload.toString] 与所有错误文案里只出现票 id 和固定占位符
///    `<redacted>`，永远不回显 secret。日志要写就写 [QrScanPayload.redacted]。
library;

import '../../../shell/contracts.dart';

/// 载荷的两段：票 id 与一次性 secret。
class QrScanPayload {
  const QrScanPayload._(this.ticketId, this.secret, this.redacted);

  /// 服务端 `create_ticket` 用的 scheme（`crates/platform/src/qr_login.rs`）。
  static const String scheme = 'wxlike-qr';

  /// secret 在日志/界面上的固定替身：**不含任何 secret 字符**。
  static const String secretPlaceholder = '<redacted>';

  final String ticketId;

  /// 一次性凭据。除了拼进 `/scan`（与后续 `/exchange`）的请求体之外，
  /// 不允许出现在任何字符串表示、日志或持久化里。
  final String secret;

  /// 可安全展示/记录的形态：`wxlike-qr://<ticket>/<redacted>`。
  final String redacted;

  @override
  String toString() => redacted;

  /// 解析入口。失败只抛 [G12FailureKind.invalidArgument]，文案可直接给用户看。
  static QrScanPayload parse(String raw) {
    // 扫码枪/剪贴板常带首尾空白，这不是"格式错误"；中间的空格才是。
    final trimmed = raw.trim();
    if (trimmed.isEmpty) throw _invalid('二维码内容为空，请重新扫描');
    final lower = trimmed.toLowerCase();
    if (!lower.startsWith('$scheme://')) {
      throw _invalid('这不是登录用的 wxlike 二维码（内容应以 $scheme:// 开头）');
    }
    final rest = trimmed.substring(scheme.length + 3);
    // query / fragment / 空白都不属于契约载荷：带上来只会拼出错误的请求，
    // 与其发出去撞 400，不如在本地说清楚。
    if (rest.contains('?') || rest.contains('#')) {
      throw _invalid('登录码格式不正确：不允许带查询参数或锚点');
    }
    if (_whitespace.hasMatch(rest)) {
      throw _invalid('登录码格式不正确：内容含空白字符');
    }
    final segments = rest.split('/');
    if (segments.length != 2) {
      throw _invalid(segments.length < 2
          ? '登录码缺少密钥，请让桌面端刷新二维码后重试'
          : '登录码格式不正确：路径段过多');
    }
    final ticketId = segments[0];
    final secret = segments[1];
    if (!_ticketPattern.hasMatch(ticketId)) {
      throw _invalid('登录码的票号无法识别，请让桌面端刷新二维码');
    }
    if (!_secretPattern.hasMatch(secret)) {
      throw _invalid('登录码的密钥无法识别，请让桌面端刷新二维码');
    }
    return QrScanPayload._(ticketId, secret, '$scheme://$ticketId/$secretPlaceholder');
  }

  /// 深链/桥接层拿到裸字符串时的唯一出口：能解析就返回，不能解析返回 null。
  static QrScanPayload? tryParse(String raw) {
    try {
      return parse(raw);
    } on G12Failure {
      return null;
    }
  }

  static G12Failure _invalid(String message) =>
      G12Failure(G12FailureKind.invalidArgument, 'QR_PAYLOAD_INVALID', message);
}

/// 票号：`identity_qr.id` 是 12 字节的 hex（24 位），留出格式演进余量。
final RegExp _ticketPattern = RegExp(r'^[0-9a-fA-F_-]{8,64}$');

/// secret：`CHALLENGE_BYTES = 24` → 48 位 hex。同样不锁死到刚好 48。
final RegExp _secretPattern = RegExp(r'^[0-9a-fA-F]{16,128}$');

/// 载荷中间不允许出现任何空白：`wxlike-qr://a/ b` 拼出来的请求是错的。
final RegExp _whitespace = RegExp(r'\s');
