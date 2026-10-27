/// 深链解析：只认本应用自己的 scheme，其余一律明确拒绝。
///
/// 形如：
/// * `wxlike://chat/<conv_id>` —— 打开会话；
/// * `wxlike://qr/<challenge_id>` —— 手机确认桌面扫码登录（只有票号，来自
///   桌面侧推送/通知，没有 secret 就**只能看不能批**）；
/// * `wxlike-qr://<ticket_id>/<secret>` —— 摄像头扫到桌面二维码后的原始载荷：
///   这才是能扫、能批的那条链路；
/// * `wxlike://contacts` —— 切到通讯录。
///
/// 解析放在壳层而不是页面里：链接是外部输入，不能让它直接构造任意路由。
/// 另外 [DeepLink.raw] 对外只能是**脱敏形态**——未知链接会被 toast 回显原文，
/// 而 `wxlike-qr://` 的原文里有 secret。
library;

import '../features/auth/qr_approval/qr_payload.dart';
import 'contracts.dart';

enum DeepLinkKind { chat, qrApproval, qrScanApproval, contacts, unknown }

class DeepLink {
  const DeepLink._(
    this.kind, {
    this.conv = '',
    this.challengeId = '',
    this.secret = '',
    this.raw = '',
  });

  final DeepLinkKind kind;
  final String conv;
  final String challengeId;
  final String secret;
  final String raw;

  bool get isKnown => kind != DeepLinkKind.unknown;

  static DeepLink parse(Uri uri) {
    final text = uri.toString();
    // 桌面二维码载荷：形状不合就 unknown（unknown 的 toast 用脱敏后的原文）。
    if (text.toLowerCase().startsWith('${QrScanPayload.scheme}://')) {
      try {
        final payload = QrScanPayload.parse(text);
        return DeepLink._(
          DeepLinkKind.qrScanApproval,
          // raw 刻意用脱敏形态：任何把 raw 显示出来的地方都不会带出 secret。
          challengeId: payload.ticketId,
          secret: payload.secret,
          raw: payload.redacted,
        );
      } on G12Failure {
        return DeepLink._(DeepLinkKind.unknown, raw: _redactQrText(text));
      }
    }
    final raw = text;
    if (uri.scheme != 'wxlike') {
      return DeepLink._(DeepLinkKind.unknown, raw: raw);
    }
    final host = uri.host;
    final rest = uri.pathSegments.where((segment) => segment.isNotEmpty).toList(growable: false);
    switch (host) {
      case 'chat':
        if (rest.length == 1 && rest.first.length <= 256) {
          return DeepLink._(DeepLinkKind.chat, conv: rest.first, raw: raw);
        }
      case 'qr':
        if (rest.length == 1 && rest.first.length <= 256) {
          return DeepLink._(DeepLinkKind.qrApproval, challengeId: rest.first, raw: raw);
        }
      case 'contacts':
        if (rest.isEmpty) return DeepLink._(DeepLinkKind.contacts, raw: raw);
    }
    return DeepLink._(DeepLinkKind.unknown, raw: raw);
  }

  /// 解析失败时的兜底：二维码载荷的最后一段就是 secret，回显之前必须换掉。
  static String _redactQrText(String text) {
    final head = '${QrScanPayload.scheme}://';
    if (!text.toLowerCase().startsWith(head)) return text;
    final rest = text.substring(head.length);
    final slash = rest.lastIndexOf('/');
    if (slash < 0) return '$head${QrScanPayload.secretPlaceholder}';
    return '$head${rest.substring(0, slash + 1)}${QrScanPayload.secretPlaceholder}';
  }
}
