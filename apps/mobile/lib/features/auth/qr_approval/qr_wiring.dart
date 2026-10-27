/// 扫码登录的组合根接线：一个服务器地址 + 一个 bearer 提供者 → 一个 [IdentityPort]。
///
/// 单独成一个文件、只有一个构造函数，是为了让"这条链路到底接没接上"这件事
/// 可以用一个测试问到实处：测试与 `main.dart` 调的是**同一个函数**，
/// 把这里的一行删掉，那个测试就红——不会出现"页面里有入口、控制器却没人构造"
/// 那种纸面上的完成。
library;

import '../../../src/service.dart';
import '../auth_ports.dart';
import 'qr_http_identity.dart';
import 'qr_transport.dart';

/// 生产 bearer 提供者。
///
/// **现状（写在这里而不是藏在实现里）**：移动端的会话 token 由 Rust 客户端持有，
/// UniFFI 绑定没有把它交给 Dart（`crates/ffi/src/client.rs` 的原话是
/// "Dart 全程碰不到 token"；`SessionInfo` 只有 `user_id / server / signed_in`）。
/// 因此默认实现拿不到 token，[requireQrBearer] 会抛
/// `QR_SESSION_TOKEN_UNAVAILABLE` 并且**不发请求**，界面显示这条真实阻塞原因，
/// 而不是假装批准成功。
///
/// 让这条链在真机上跑通的下一步在 `crates/ffi`：导出一个带鉴权的
/// `api_request(method, path, body_json)`（或直接 `session_token()`），
/// 然后在这里换成"问 Rust 要 bearer"。那不在本卡的文件所有权内。
///
/// [override] 是给宿主/调试留的唯一注入口；为空即"没有 token"。
QrTokenProvider defaultQrTokenProvider({String override = ''}) {
  if (override.trim().isNotEmpty) {
    final fixed = override.trim();
    return () async => fixed;
  }
  return () async => null;
}

/// 构造扫码登录用的身份端口。
///
/// [transport] 为空时用 [HttpQrApiTransport]（真发 HTTP）；测试传一个记录型
/// 传输，其余路径完全一样。
///
/// [token] 同时交给**端口与传输**：端口那一份是"没凭据就不发请求"的策略
/// （[requireQrBearer]），传给它的是哪一个传输都拦得住；传输那一份才是真去
/// 拼 `Authorization` 头。少给端口那一份，注入记录型传输的测试就再也问不到
/// 这条策略——那正是"纸面完成"的形状。
IdentityPort buildQrIdentityPort({
  required String Function() serverBase,
  QrTokenProvider? token,
  SessionInfo? Function()? sessionProvider,
  IdentityPort? fallback,
  QrApiTransport? transport,
}) {
  final bearer = token ?? defaultQrTokenProvider();
  return HttpQrIdentityPort(
    transport: transport ?? HttpQrApiTransport(baseUrl: serverBase, token: bearer),
    token: bearer,
    fallback: fallback,
    sessionProvider: sessionProvider,
  );
}
