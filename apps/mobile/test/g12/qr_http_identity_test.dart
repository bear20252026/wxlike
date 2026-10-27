import 'package:flutter_test/flutter_test.dart';
import 'package:wxlike_mobile/features/auth/auth_models.dart';
import 'package:wxlike_mobile/features/auth/qr_approval/qr_http_identity.dart';
import 'package:wxlike_mobile/features/auth/qr_approval/qr_transport.dart';
import 'package:wxlike_mobile/shell/contracts.dart';
import 'package:wxlike_mobile/src/service.dart';

import 'qr_support.dart';

/// 路线契约的形状断言（`crates/platform/src/qr_login.rs` 的模块注释就是这套）。
void main() {
  const scannedReply = {
    'ticket_id': qrTicketId,
    'state': 'SCANNED',
    'platform': 'web',
    'label': 'MacBook Pro',
    'ip_hint': '203.0.113.7',
  };

  HttpQrIdentityPort port(RecordingQrTransport transport) => HttpQrIdentityPort(
        transport: transport,
        sessionProvider: () =>
            const SessionInfo(userId: 'u1', server: 'http://srv', signedIn: true),
      );

  group('HttpQrIdentityPort 的请求形状', () {
    test('scan 只 POST /scan，body 就是 {secret}', () async {
      final transport = RecordingQrTransport(
        reply: (_) => const QrApiResponse(status: 200, body: scannedReply),
      );
      final challenge = await port(transport).scanQr(qrTicketId, secret: qrSecret);

      expect(transport.requests, hasLength(1));
      final request = transport.requests.single;
      expect(request.method, 'POST');
      expect(request.path, '/api/qr/tickets/$qrTicketId/scan');
      // 这一行是本卡的核心断言：少了它，"body 里到底有没有 secret"没人管。
      expect(request.body, {'secret': qrSecret});
      expect(request.authRequired, isTrue);
      expect(challenge.status, QrChallengeStatus.scanned);
      expect(challenge.contractState, 'SCANNED');
      expect(challenge.deviceName, 'MacBook Pro');
      expect(challenge.deviceKind, 'web');
      expect(challenge.location, '203.0.113.7');
    });

    test('approve 打 /approve，不带 body，带幂等键', () async {
      final transport = RecordingQrTransport(
        reply: (_) => const QrApiResponse(
          status: 200,
          body: {'ticket_id': qrTicketId, 'state': 'APPROVED'},
        ),
      );
      final challenge = await port(transport)
          .approveQr(qrTicketId, idempotencyKey: 'qr-approve-1');

      final request = transport.requests.single;
      expect(request.method, 'POST');
      expect(request.path, '/api/qr/tickets/$qrTicketId/approve');
      expect(request.body, isNull, reason: '批准没有 body，多塞一个就是改契约');
      expect(request.idempotencyKey, 'qr-approve-1');
      expect(challenge.status, QrChallengeStatus.approved);
      expect(challenge.contractState, 'APPROVED');
    });

    test('reject 打 /reject 并把 REJECTED 带回来', () async {
      final transport = RecordingQrTransport(
        reply: (_) => const QrApiResponse(
          status: 200,
          body: {'ticket_id': qrTicketId, 'state': 'REJECTED'},
        ),
      );
      final challenge =
          await port(transport).rejectQr(qrTicketId, idempotencyKey: 'k');
      expect(transport.requests.single.path, '/api/qr/tickets/$qrTicketId/reject');
      expect(challenge.status, QrChallengeStatus.rejected);
      expect(challenge.contractState, 'REJECTED');
    });

    test('GET 轮询只有 {state, account_hint}：不假装有 secret 和到期时间', () async {
      final transport = RecordingQrTransport(
        reply: (_) => const QrApiResponse(
          status: 200,
          // 服务端 GET 的真实回包形状——之前有个桌面 e2e 断言就是因为
          // 以为这里有 secret 而空转了。这里明确断言"读不到"。
          body: {'state': 'SCANNED', 'account_hint': 'alice'},
        ),
      );
      final challenge = await port(transport).qrChallenge(qrTicketId);

      expect(transport.requests.single.method, 'GET');
      expect(transport.requests.single.path, '/api/qr/tickets/$qrTicketId');
      expect(transport.requests.single.body, isNull);
      expect(challenge.status, QrChallengeStatus.scanned);
      expect(challenge.scannedBy, 'alice');
      // 到期时间只在建票响应里，所以扫码侧必须承认"不知道"。
      expect(challenge.deadlineKnown, isFalse);
    });

    test('认不出的状态一律当过期，不给批准的机会', () async {
      final transport = RecordingQrTransport(
        reply: (_) => const QrApiResponse(status: 200, body: {'state': 'WEIRD'}),
      );
      final challenge = await port(transport).qrChallenge(qrTicketId);
      expect(challenge.status, QrChallengeStatus.expired);
    });

    test('票号要过白名单，畸形票号不发请求', () async {
      final transport = RecordingQrTransport();
      final failure = await guard(() => port(transport).scanQr('../../etc/passwd', secret: qrSecret));
      expect(failure.code, 'QR_TICKET_ID_INVALID');
      expect(transport.requests, isEmpty);
    });
  });

  group('HttpQrIdentityPort 的会话前置', () {
    test('本机没登录就不发请求，并且说清原因', () async {
      final transport = RecordingQrTransport();
      final guarded = HttpQrIdentityPort(
        transport: transport,
        sessionProvider: () =>
            const SessionInfo(userId: '', server: 'http://srv', signedIn: false),
      );
      final failure = await guard(() => guarded.scanQr(qrTicketId, secret: qrSecret));
      expect(failure.code, 'QR_SESSION_INVALID');
      expect(transport.requests, isEmpty);
    });

    test('空 secret 直接拒绝，不打网络', () async {
      final transport = RecordingQrTransport();
      final failure = await guard(() => port(transport).scanQr(qrTicketId, secret: '  '));
      expect(failure.code, 'QR_SECRET_MISSING');
      expect(transport.requests, isEmpty);
    });
  });

  group('HTTP 状态 → 界面语言', () {
    Future<G12Failure> fail(int status, Map<String, Object?> body) async {
      final transport = RecordingQrTransport(
        reply: (_) => QrApiResponse(status: status, body: body),
      );
      return guard(() => port(transport).approveQr(qrTicketId, idempotencyKey: 'k'));
    }

    test('401 是登录失效、403 是不该管、409 是状态变了、410 是过期', () async {
      expect((await fail(401, const {'error': 'missing bearer token'})).kind,
          G12FailureKind.unauthorized);
      expect((await fail(403, const {'error': 'not your ticket'})).kind,
          G12FailureKind.forbidden);
      expect((await fail(409, const {'error': 'challenge not scanned'})).code,
          'QR_TICKET_STATE_CONFLICT');
      expect((await fail(410, const {})).code, 'QR_TICKET_EXPIRED');
      expect((await fail(404, const {})).code, 'QR_TICKET_NOT_FOUND');
      expect((await fail(429, const {})).retryable, isTrue);
      expect((await fail(500, const {'error': 'database failure'})).message,
          'database failure');
    });

    test('服务端错误文案里不会带上 secret', () async {
      final transport = RecordingQrTransport(
        reply: (_) => QrApiResponse(
          status: 422,
          body: {'error': 'bad input: $qrSecret leaked'},
        ),
      );
      final failure = await guard(() => port(transport).scanQr(qrTicketId, secret: qrSecret));
      // 这不是"我们打印了"，而是"服务端把 secret 回给了错误文案"——
      // 传输层只取 error 字段，所以必须在端口这一层截掉。
      expect(failure.displayText, isNot(contains(qrSecret)));
    });
  });
}
