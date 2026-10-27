/// 扫码登录用到的那条 JSON/HTTP 通道。
///
/// **为什么单独开一个缝而不是再造一套 HTTP**：移动端只有一套传输——
/// `lib/src/service.dart` 里那个跑在 worker isolate 上的 Rust 客户端
/// （ureq + bearer）。这一层不解析业务、不做判断，只把"方法 + 路径 + JSON body"
/// 交给注入进来的 [QrApiTransport]；生产实现是 [HttpQrApiTransport]，
/// 测试实现是记录型假传输。请求形状只在这里拼一次，端口实现只管契约字段。
///
/// # 与 `crates/platform/src/qr_login.rs` 的路线契约
///
/// | 方法 | 路径 | 鉴权 | body |
/// |------|------|------|------|
/// | GET | `/api/qr/tickets/{id}` | Bearer **创建者** | — |
/// | POST | `/api/qr/tickets/{id}/scan` | Bearer 扫码者 | `{secret}` |
/// | POST | `/api/qr/tickets/{id}/approve` | Bearer 扫码者 | — |
/// | POST | `/api/qr/tickets/{id}/reject` | Bearer 扫码者 | — |
///
/// 两处踩过坑的形状细节写死在这里，防止"测试假绿"：
/// * `GET` 的回包**只有** `{state, account_hint?}`——`secret` 只出现在建票响应里，
///   任何"从 GET 里读 secret"的断言都是空转；
/// * `approve` 之前必须 `scan`：identity 状态机要求 `pending → scanned → approved`，
///   没扫过的票直接批准会 409。
library;

import 'dart:async';
import 'dart:convert';
import 'dart:io';

import '../../../shell/contracts.dart';

/// 一次要发出的请求。
class QrApiRequest {
  const QrApiRequest({
    required this.method,
    required this.path,
    this.body,
    this.idempotencyKey = '',
    this.authRequired = true,
  });

  final String method;
  final String path;

  /// body 里可能含一次性凭据；这是它在任何文案里唯一的替身。
  static const String redactedPlaceholder = '<redacted>';

  /// 可能含 secret。**因此 [toString] 绝不带上它**，日志只允许出现方法与路径。
  final Map<String, Object?>? body;
  final String idempotencyKey;

  /// `false` 表示匿名端点（本卡只有 `/exchange` 是，扫码与批准都要 bearer）。
  final bool authRequired;

  @override
  String toString() => '$method $path';
}

class QrApiResponse {
  const QrApiResponse({required this.status, required this.body});

  final int status;
  final Map<String, Object?> body;

  bool get ok => status >= 200 && status < 300;

  /// 服务端错误体是 `{"error": "…"}`（`crates/api`：ApiError 的唯一形状）。
  String get serverMessage {
    final error = body['error'];
    return error is String ? error : '';
  }

  @override
  String toString() => 'QrApiResponse($status)';
}

/// 现有会话的 bearer 提供者；没有登录态时返回 null（不是空串）。
typedef QrTokenProvider = Future<String?> Function();

/// 传输缝：唯一的 I/O 出口。
abstract class QrApiTransport {
  Future<QrApiResponse> send(QrApiRequest request);

  void dispose();
}

/// 把 HTTP 状态码翻成界面认识的 [G12Failure]。
///
/// 消息只取服务端的 `error` 文案（`qr_login.rs` 的注释确认它不含凭据），
/// 永远不带请求体——请求体里有 secret。
G12Failure qrFailureFrom(QrApiResponse response) {
  final detail = response.serverMessage;
  return switch (response.status) {
    400 || 422 => G12Failure(
        G12FailureKind.invalidArgument,
        'QR_TICKET_BAD_REQUEST',
        detail.isEmpty ? '登录码无效，请让桌面端刷新二维码' : detail,
      ),
    401 => const G12Failure(
        G12FailureKind.unauthorized,
        'QR_SESSION_INVALID',
        '本机登录态已失效，请重新登录后再扫码',
      ),
    403 => const G12Failure(
        G12FailureKind.forbidden,
        'QR_TICKET_FORBIDDEN',
        '这张登录码不属于当前会话，无法确认',
      ),
    404 => const G12Failure(
        G12FailureKind.invalidArgument,
        'QR_TICKET_NOT_FOUND',
        '找不到这张登录码，请让桌面端刷新二维码',
      ),
    409 => G12Failure(
        G12FailureKind.conflict,
        'QR_TICKET_STATE_CONFLICT',
        detail.isEmpty ? '登录码状态已变化，请让桌面端重新生成' : detail,
        retryable: true,
      ),
    410 => const G12Failure(
        G12FailureKind.conflict,
        'QR_TICKET_EXPIRED',
        '登录码已过期，请让桌面端刷新二维码',
      ),
    429 => const G12Failure(
        G12FailureKind.unavailable,
        'QR_RATE_LIMITED',
        '操作太频繁，请稍后再试',
        retryable: true,
      ),
    _ => G12Failure(
        G12FailureKind.unexpected,
        'QR_HTTP_${response.status}',
        detail.isEmpty ? '服务端返回 ${response.status}' : detail,
      ),
  };
}

/// 生产实现：`dart:io HttpClient` 打平台 HTTP 面。
///
/// bearer 来自注入的 [QrTokenProvider]。当前绑定没有把 token 交给 Dart
/// （`crates/ffi/src/client.rs`："Dart 全程碰不到 token"），所以组合根给出的
/// 提供者拿不到凭据时，这里抛 `QR_SESSION_TOKEN_UNAVAILABLE` 而**不发请求**——
/// 界面上会显示这条阻塞原因，而不是假装批准成功。要让它真跑通，需要
/// `crates/ffi` 导出一个带鉴权的 api_request（本卡文件所有权之外）。
class HttpQrApiTransport implements QrApiTransport {
  HttpQrApiTransport({
    required this.baseUrl,
    required this.token,
    HttpClient Function()? createClient,
    this.timeout = const Duration(seconds: 10),
  })  : _clientFactory = createClient ?? HttpClient.new;

  final String Function() baseUrl;
  final QrTokenProvider token;
  final HttpClient Function() _clientFactory;
  final Duration timeout;

  HttpClient? _client;

  @override
  Future<QrApiResponse> send(QrApiRequest request) async {
    final base = baseUrl().trim();
    if (base.isEmpty) {
      throw const G12Failure(
        G12FailureKind.invalidArgument,
        'QR_SERVER_MISSING',
        '还没有配置服务器地址',
      );
    }
    String? bearer;
    if (request.authRequired) {
      bearer = await token();
      if (bearer == null || bearer.trim().isEmpty) {
        throw const G12Failure(
          G12FailureKind.unavailable,
          'QR_SESSION_TOKEN_UNAVAILABLE',
          '本机登录凭据仍在 Rust 侧，移动端绑定尚未导出可用 token，无法代发这个请求',
        );
      }
      // 头注入防线：bearer 带 CR/LF 就是往请求头里塞内容（与 Rust 侧 usable_token 同口径）。
      if (bearer.contains('\r') || bearer.contains('\n')) {
        throw const G12Failure(
          G12FailureKind.unauthorized,
          'QR_SESSION_INVALID',
          '本机登录态已失效，请重新登录后再扫码',
        );
      }
    }

    // path 由端口构造，总是以 `/` 开头（`/api/qr/tickets/…`）；base 去掉尾斜杠，
    // 拼出双斜杠会让服务端路由 404，那种 404 很容易被当成"票不存在"。
    final uri = Uri.parse('${_join(base)}${request.path}');
    final client = _client ??= _clientFactory();
    try {
      final req = await client.openUrl(request.method, uri)
        ..followRedirects = false
        ..persistentConnection = false;
      req.headers.set(HttpHeaders.acceptHeader, 'application/json');
      if (bearer != null) req.headers.set(HttpHeaders.authorizationHeader, 'Bearer $bearer');
      if (request.idempotencyKey.isNotEmpty) {
        req.headers.set('idempotency-key', request.idempotencyKey);
      }
      final body = request.body;
      if (body != null) {
        req.headers.contentType = ContentType.json;
        // JSON 序列化默认转义非 ASCII；secret 是 hex，转不转都一样，
        // 但绝不把 body 写进任何异常消息里。
        req.add(utf8.encode(jsonEncode(body)));
      }
      final res = await req.close().timeout(timeout);
      final text = await res.transform(utf8.decoder).join().timeout(timeout);
      return QrApiResponse(status: res.statusCode, body: _decode(text));
    } on SocketException {
      throw const G12Failure(G12FailureKind.offline, 'QR_OFFLINE', '网络已断开，请检查连接');
    } on TimeoutException {
      throw const G12Failure(G12FailureKind.timeout, 'QR_TIMEOUT', '请求超时，可以重试', retryable: true);
    } on HttpException {
      throw const G12Failure(G12FailureKind.unexpected, 'QR_HTTP_BAD_RESPONSE', '服务端返回了无法解析的响应');
    }
  }

  @override
  void dispose() {
    _client?.close(force: true);
    _client = null;
  }

  static Map<String, Object?> _decode(String text) {
    if (text.trim().isEmpty) return const {};
    try {
      final decoded = jsonDecode(text);
      return decoded is Map ? Map<String, Object?>.from(decoded) : const {};
    } on FormatException {
      return const {};
    }
  }

  static String _join(String base) => base.endsWith('/') ? base.substring(0, base.length - 1) : base;
}
