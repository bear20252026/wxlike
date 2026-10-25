/// 深链解析：只认本应用自己的 scheme，其余一律明确拒绝。
///
/// 形如：
/// * `wxlike://chat/<conv_id>` —— 打开会话；
/// * `wxlike://qr/<challenge_id>` —— 手机确认桌面扫码登录；
/// * `wxlike://contacts` —— 切到通讯录。
///
/// 解析放在壳层而不是页面里：链接是外部输入，不能让它直接构造任意路由。
library;

enum DeepLinkKind { chat, qrApproval, contacts, unknown }

class DeepLink {
  const DeepLink._(this.kind, {this.conv = '', this.challengeId = '', this.raw = ''});

  final DeepLinkKind kind;
  final String conv;
  final String challengeId;
  final String raw;

  bool get isKnown => kind != DeepLinkKind.unknown;

  static DeepLink parse(Uri uri) {
    final raw = uri.toString();
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
}
