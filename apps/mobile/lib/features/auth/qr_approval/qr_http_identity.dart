/// 扫码登录的 HTTP 端口实现：把 `identity.qr_login.*` 的三个动作接到
/// [QrApiTransport] 上，形状严格按 `crates/platform/src/qr_login.rs` 的路线契约。
///
/// 与 [UnavailableIdentityPort] 同一层定位（它是"上游没接线就明确报错"，本类是
/// "上游是 HTTP"），所以它实现的是同一个 [IdentityPort]：控制器与页面不认识 HTTP，
/// 换实现只动组合根。
///
/// # 三条不能松的规矩
/// 1. `secret` 只进 `/scan` 的请求体。GET 与 approve/reject 都不带它，异常与
///    [QrApiRequest.toString] 也不带它，所以它不可能从这条路漏进日志。
/// 2. **先扫后批**。identity 的 `decide()` 要求 `state = 'scanned'`，直接批准必 409。
/// 3. 回包字段缺失就当没发生：`GET /api/qr/tickets/{id}` 只有 `{state, account_hint}`
///    （`secret`、`expires_at_ms` 都只在建票响应里），所以这里不假装知道到期时间，
///    而是把 `deadlineKnown` 置 false 交给界面显示。
library;

import '../../../shell/contracts.dart';
import '../../../src/service.dart';
import '../auth_models.dart';
import '../auth_ports.dart';
import 'qr_transport.dart';

/// 票 id 是路径段，先过一层白名单再拼 URL。
final RegExp _pathSafeId = RegExp(r'^[0-9a-zA-Z_-]{1,128}$');

class HttpQrIdentityPort implements IdentityPort {
  HttpQrIdentityPort({
    required this.transport,
    IdentityPort? fallback,
    this.sessionProvider,
  })  : // 短信/协议那半边本卡不接：继续沿用组合根给的端口（默认
        // [UnavailableIdentityPort]，每个入口都明确报错，绝不伪造验证码流程）。
        _fallback = fallback ?? const UnavailableIdentityPort();

  final QrApiTransport transport;
  final SessionInfo? Function()? sessionProvider;
  final IdentityPort _fallback;

  // ------------------------------------------------------------------
  // 扫码登录
  // ------------------------------------------------------------------

  @override
  Future<QrLoginChallenge> qrChallenge(String challengeId) async {
    // 创建者轮询面。扫码者调它拿到的会是 403（`not your ticket`）——那是服务端的
    // 答案，这里原样转成 G12Failure 显示出来，不猜。
    final reply = await _send(QrApiRequest(method: 'GET', path: _ticketPath(challengeId)));
    return _challengeFrom(challengeId, reply.body);
  }

  @override
  Future<QrLoginChallenge> scanQr(String challengeId, {required String secret}) async {
    if (secret.trim().isEmpty) {
      throw const G12Failure(
        G12FailureKind.invalidArgument,
        'QR_SECRET_MISSING',
        '登录码缺少密钥，请让桌面端刷新二维码后重试',
      );
    }
    final reply = await _send(
      QrApiRequest(
        method: 'POST',
        // 契约：secret 放在 body，不在路径里（路径是票 id）。
        path: '${_ticketPath(challengeId)}/scan',
        // 这就是唯一的出口：整条链路上 secret 只出现在这一个 map 里。
        body: {'secret': secret},
      ),
    );
    return _challengeFrom(challengeId, reply.body);
  }

  @override
  Future<QrLoginChallenge> approveQr(String challengeId, {required String idempotencyKey}) async {
    final reply = await _send(
      QrApiRequest(
        method: 'POST',
        path: '${_ticketPath(challengeId)}/approve',
        idempotencyKey: idempotencyKey,
      ),
    );
    return _challengeFrom(challengeId, reply.body);
  }

  @override
  Future<QrLoginChallenge> rejectQr(String challengeId, {required String idempotencyKey}) async {
    final reply = await _send(
      QrApiRequest(
        method: 'POST',
        path: '${_ticketPath(challengeId)}/reject',
        idempotencyKey: idempotencyKey,
      ),
    );
    return _challengeFrom(challengeId, reply.body);
  }

  // ------------------------------------------------------------------
  // 其余身份入口：不由本端口实现（见构造函数注释）
  // ------------------------------------------------------------------

  @override
  Future<List<ConsentDocument>> consentDocuments({String locale = 'zh'}) =>
      _fallback.consentDocuments(locale: locale);

  @override
  Future<PhoneChallenge> requestSmsCode({
    required String dialCode,
    required String nationalNumber,
    required String idempotencyKey,
  }) =>
      _fallback.requestSmsCode(
        dialCode: dialCode,
        nationalNumber: nationalNumber,
        idempotencyKey: idempotencyKey,
      );

  @override
  Future<PhoneChallenge> resendSmsCode(String challengeId, {required String idempotencyKey}) =>
      _fallback.resendSmsCode(challengeId, idempotencyKey: idempotencyKey);

  @override
  Future<PhoneChallenge> challenge(String challengeId) => _fallback.challenge(challengeId);

  @override
  Future<SessionInfo> verifySmsCode(
    String challengeId,
    String code, {
    required List<ConsentAcceptance> consents,
    required String idempotencyKey,
  }) =>
      _fallback.verifySmsCode(
        challengeId,
        code,
        consents: consents,
        idempotencyKey: idempotencyKey,
      );

  // ------------------------------------------------------------------

  Future<QrApiResponse> _send(QrApiRequest request) async {
    final current = sessionProvider?.call();
    if (current != null && !current.signedIn) {
      throw const G12Failure(
        G12FailureKind.unauthorized,
        'QR_SESSION_INVALID',
        '本机尚未登录，无法替桌面确认登录',
      );
    }
    final reply = await transport.send(request);
    if (!reply.ok) throw _scrub(qrFailureFrom(reply), request);
    return reply;
  }

  /// 服务端把请求内容回显在错误文案里是现实行为（422 `bad input: …`），而这里的
  /// 请求体就是 secret。任何回显都必须在进界面、进 `toString`、进日志之前换掉——
  /// 错误文案是这条链路上唯一能把凭据带出去的门。
  static G12Failure _scrub(G12Failure failure, QrApiRequest request) {
    final sensitive = (request.body ?? const <String, Object?>{})
        .values
        .whereType<String>()
        .where((value) => value.length >= 8)
        .toList();
    if (sensitive.isEmpty) return failure;
    var message = failure.message;
    for (final value in sensitive) {
      message = message.replaceAll(value, QrApiRequest.redactedPlaceholder);
    }
    if (message == failure.message) return failure;
    return G12Failure(
      failure.kind,
      failure.code,
      message,
      retryable: failure.retryable,
      operationId: failure.operationId,
    );
  }

  static String _ticketPath(String ticketId) {
    if (!_pathSafeId.hasMatch(ticketId)) {
      throw G12Failure(
        G12FailureKind.invalidArgument,
        'QR_TICKET_ID_INVALID',
        '登录码的票号无法识别，请让桌面端刷新二维码',
      );
    }
    return '/api/qr/tickets/${Uri.encodeComponent(ticketId)}';
  }

  /// 回包 → 视图模型。
  ///
  /// `approve`/`reject` 只回 `{ticket_id, state}`，设备名/平台/线索是 `/scan`
  /// 给的；这里不猜，缺就留空，由 [QrApprovalController] 把上一次看到的展示字段
  /// 带下来（见它的 `_merge`）。
  static QrLoginChallenge _challengeFrom(String ticketId, Map<String, Object?> body) {
    final state = '${body['state'] ?? ''}'.toUpperCase();
    final now = DateTime.now().toUtc();
    final id = '${body['ticket_id'] ?? ''}'.isEmpty ? ticketId : '${body['ticket_id']}';
    final label = _text(body['label']);
    final hint = _text(body['account_hint']);
    final platform = _text(body['platform']);
    final ipHint = _text(body['ip_hint']);
    return QrLoginChallenge(
      id: id,
      // 建票时带的 label 就是桌面设备名；轮询面只给 account_hint（扫码者是谁）。
      deviceName: label.isNotEmpty ? label : id,
      deviceKind: platform.isNotEmpty ? platform : '',
      location: ipHint,
      issuedAtUtc: now,
      expiresAtUtc: now,
      status: _statusOf(state),
      scannedBy: hint,
      // 扫码侧看不到 expires_at：到期由服务端裁定。
      deadlineKnown: false,
    );
  }

  /// 契约状态名 → 视图枚举。认不出来的状态一律 `expired`：宁可让用户重新扫，
  /// 也不能把"看不懂"当成"可以批准"。
  static QrChallengeStatus _statusOf(String state) => switch (state) {
        'CREATED' => QrChallengeStatus.pending,
        'PENDING' => QrChallengeStatus.pending,
        'SCANNED' => QrChallengeStatus.scanned,
        'WAITING_APPROVAL' => QrChallengeStatus.scanned,
        'APPROVED' => QrChallengeStatus.approved,
        'REJECTED' => QrChallengeStatus.rejected,
        'CONSUMED' => QrChallengeStatus.consumed,
        'EXPIRED' => QrChallengeStatus.expired,
        _ => QrChallengeStatus.expired,
      };

  static String _text(Object? value) => value is String ? value.trim() : '';
}
