import 'package:flutter_test/flutter_test.dart';
import 'package:wxlike_mobile/features/auth/auth_controller.dart';
import 'package:wxlike_mobile/features/auth/auth_models.dart';
import 'package:wxlike_mobile/features/auth/auth_ports.dart';
import 'package:wxlike_mobile/shell/contracts.dart';
import 'package:wxlike_mobile/src/service.dart';

import 'support.dart';

class FakeClock implements MonotonicClock {
  Duration value = Duration.zero;

  @override
  Duration elapsed() => value;

  void advance(Duration by) => value += by;
}

class FakeIdentity implements IdentityPort {
  FakeIdentity({required this.now});

  final DateTime Function() now;

  final List<String> requested = [];
  int requests = 0;
  int resends = 0;
  int verifies = 0;
  final List<String> verifyKeys = [];
  Object? requestError;
  Object? verifyError;
  SmsDelivery delivery = SmsDelivery.sms;

  PhoneChallenge? active;
  QrLoginChallenge? qr;

  @override
  Future<List<ConsentDocument>> consentDocuments({
    String locale = 'zh',
  }) async => [
    ConsentDocument(
      id: 'terms',
      title: '服务协议',
      version: '2.1',
      requiredToLogin: true,
    ),
    ConsentDocument(
      id: 'privacy',
      title: '隐私政策',
      version: '1.9',
      requiredToLogin: true,
    ),
  ];

  @override
  Future<PhoneChallenge> requestSmsCode({
    required String dialCode,
    required String nationalNumber,
    required String idempotencyKey,
  }) async {
    requests++;
    requested.add('$dialCode:$nationalNumber');
    if (requestError != null) throw requestError!;
    return active = PhoneChallenge(
      id: 'ch-1',
      maskedPhone: '138****0000',
      delivery: delivery,
      resendAtUtc: now().add(const Duration(seconds: 30)),
      expiresAtUtc: now().add(const Duration(minutes: 5)),
    );
  }

  @override
  Future<PhoneChallenge> resendSmsCode(
    String challengeId, {
    required String idempotencyKey,
  }) async {
    resends++;
    return active = PhoneChallenge(
      id: challengeId,
      maskedPhone: '138****0000',
      delivery: delivery,
      resendAtUtc: now().add(const Duration(seconds: 30)),
      expiresAtUtc: now().add(const Duration(minutes: 5)),
    );
  }

  @override
  Future<PhoneChallenge> challenge(String challengeId) async {
    final current = active;
    if (current == null) {
      throw const G12Failure(
        G12FailureKind.invalidArgument,
        'CODE_EXPIRED',
        '挑战已过期',
      );
    }
    return current;
  }

  @override
  Future<SessionInfo> verifySmsCode(
    String challengeId,
    String code, {
    required List<ConsentAcceptance> consents,
    required String idempotencyKey,
  }) async {
    verifies++;
    verifyKeys.add(idempotencyKey);
    if (verifyError != null) throw verifyError!;
    return const SessionInfo(userId: 'u1', server: 's', signedIn: true);
  }

  @override
  Future<QrLoginChallenge> qrChallenge(String challengeId) async {
    final current = qr;
    if (current == null) {
      throw const G12Failure(
        G12FailureKind.invalidArgument,
        'CODE_EXPIRED',
        '二维码已过期',
      );
    }
    return current;
  }

  @override
  Future<QrLoginChallenge> approveQr(
    String challengeId, {
    required String idempotencyKey,
  }) async {
    final current = qr!;
    return qr = QrLoginChallenge(
      id: current.id,
      deviceName: current.deviceName,
      deviceKind: current.deviceKind,
      location: current.location,
      issuedAtUtc: current.issuedAtUtc,
      expiresAtUtc: current.expiresAtUtc,
      status: QrChallengeStatus.approved,
      scannedBy: 'me',
    );
  }

  @override
  Future<QrLoginChallenge> rejectQr(
    String challengeId, {
    required String idempotencyKey,
  }) async {
    final current = qr!;
    return qr = QrLoginChallenge(
      id: current.id,
      deviceName: current.deviceName,
      deviceKind: current.deviceKind,
      location: current.location,
      issuedAtUtc: current.issuedAtUtc,
      expiresAtUtc: current.expiresAtUtc,
      status: QrChallengeStatus.rejected,
    );
  }
}

void main() {
  group('AuthController 手机号挑战', () {
    late FakeClock clock;
    late FakeIdentity identity;
    late MemorySecrets secrets;
    late DateTime now;

    AuthController build() => AuthController(
      identity: identity,
      secrets: AuthSecretStore(secrets),
      clock: clock,
      nowUtc: () => now,
    );

    setUp(() {
      clock = FakeClock();
      now = DateTime.utc(2026, 10, 24, 9);
      identity = FakeIdentity(now: () => now);
      secrets = MemorySecrets();
    });

    test('手机号未通过校验时不发请求', () async {
      final controller = build();
      controller.setPhone('123');
      expect(await controller.requestCode(), isFalse);
      expect(controller.failure?.code, 'PHONE_INVALID');
      expect(identity.requests, 0);
    });

    test('倒计时来自服务端 deadline 与单调时钟，重启后仍受限', () async {
      final controller = build();
      controller.setPhone('13800000000');
      expect(await controller.requestCode(), isTrue);
      expect(identity.requested.single, '86:13800000000');
      expect(controller.remaining.inSeconds, 30);
      expect(controller.canRequestCode, isFalse);

      clock.advance(const Duration(seconds: 29));
      expect(controller.remaining.inSeconds, 1);

      // 进程重启：新控制器、单调时钟归零，但服务端 deadline 仍然生效。
      now = now.add(const Duration(seconds: 29));
      await secrets.write(loginChallengeSecretKey, 'ch-1');
      final restarted = build();
      restarted.setPhone('13800000000');
      await restarted.restore();
      expect(restarted.challenge?.id, 'ch-1');
      expect(restarted.remaining.inSeconds, 1);
      expect(restarted.canRequestCode, isFalse);

      clock.advance(const Duration(seconds: 1));
      expect(restarted.canRequestCode, isTrue);
    });

    test('重发沿用同一个挑战，不新建第二条', () async {
      final controller = build();
      controller.setPhone('13800000000');
      await controller.requestCode();
      clock.advance(const Duration(seconds: 30));
      expect(await controller.requestCode(resend: true), isTrue);
      expect(identity.requests, 1);
      expect(identity.resends, 1);
    });

    test('协议未齐不能验证；勾选后提交版本号', () async {
      final controller = build();
      await controller.loadDocuments();
      controller.setPhone('13800000000');
      await controller.requestCode();
      controller.setCode('123456');
      expect(controller.canVerify, isFalse);

      controller.toggleConsent('terms', true);
      expect(controller.canVerify, isFalse);
      controller.toggleConsent('privacy', true);
      expect(controller.canVerify, isTrue);

      final session = await controller.verify();
      expect(session?.signedIn, isTrue);
      expect(identity.verifies, 1);
    });

    test('网络失败复用幂等键，验证码错误换新键', () async {
      final controller = build();
      await controller.loadDocuments();
      controller.setPhone('13800000000');
      await controller.requestCode();
      controller.setCode('123456');
      controller.toggleConsent('terms', true);
      controller.toggleConsent('privacy', true);

      identity.verifyError = const G12Failure(
        G12FailureKind.offline,
        'NETWORK',
        '断网',
        retryable: true,
      );
      expect(await controller.verify(), isNull);
      identity.verifyError = null;
      expect(await controller.verify(), isNotNull);
      expect(identity.verifyKeys.length, 2);
      expect(identity.verifyKeys[0], identity.verifyKeys[1]);

      // 第二次登录：错误码是终态，下一次提交必须换 key。
      await controller.requestCode();
      controller.setCode('654321');
      identity.verifyError = const G12Failure(
        G12FailureKind.invalidArgument,
        'CODE_INVALID',
        '验证码错误',
      );
      expect(await controller.verify(), isNull);
      identity.verifyError = null;
      expect(await controller.verify(), isNotNull);
      expect(identity.verifyKeys[2], isNot(identity.verifyKeys[1]));
    });

    test('协议文档未加载时不允许提交，避免无同意登录', () async {
      final controller = build();
      controller.setPhone('13800000000');
      await controller.requestCode();
      controller.setCode('123456');
      expect(controller.canVerify, isFalse);
      expect(await controller.verify(), isNull);
      expect(identity.verifies, 0);
    });

    test('短信通道不可用时如实报错，不进入可验证状态', () async {
      identity.delivery = SmsDelivery.unavailable;
      final controller = build();
      controller.setPhone('13800000000');
      expect(await controller.requestCode(), isFalse);
      expect(controller.failure?.code, 'SMS_UNAVAILABLE');
      expect(controller.canVerify, isFalse);
    });
  });

  group('QrApprovalController', () {
    test('只批准待处理且未过期的挑战，离页后不能再批准', () async {
      final clock = FakeClock();
      var now = DateTime.utc(2026, 10, 24, 9);
      final identity = FakeIdentity(now: () => now);
      identity.qr = QrLoginChallenge(
        id: 'qr-1',
        deviceName: 'MacBook Pro',
        deviceKind: 'desktop',
        location: '上海',
        issuedAtUtc: now,
        expiresAtUtc: now.add(const Duration(minutes: 2)),
        status: QrChallengeStatus.pending,
      );
      final controller = QrApprovalController(
        identity: identity,
        challengeId: 'qr-1',
        clock: clock,
        nowUtc: () => now,
      );
      await controller.load();
      expect(controller.canDecide, isTrue);

      expect(await controller.approve(), isTrue);
      expect(controller.challenge?.status, QrChallengeStatus.approved);
      expect(controller.canDecide, isFalse);

      controller.dispose();
      expect(controller.canDecide, isFalse);
      expect(await controller.reject(), isFalse);
    });

    test('过期挑战不可批准', () async {
      final clock = FakeClock();
      final now = DateTime.utc(2026, 10, 24, 9);
      final identity = FakeIdentity(now: () => now);
      identity.qr = QrLoginChallenge(
        id: 'qr-2',
        deviceName: 'PC',
        deviceKind: 'desktop',
        location: '',
        issuedAtUtc: now.subtract(const Duration(minutes: 5)),
        expiresAtUtc: now.subtract(const Duration(minutes: 1)),
        status: QrChallengeStatus.pending,
      );
      final controller = QrApprovalController(
        identity: identity,
        challengeId: 'qr-2',
        clock: clock,
        nowUtc: () => now,
      );
      await controller.load();
      expect(controller.remaining, Duration.zero);
      expect(controller.canDecide, isFalse);
      expect(await controller.approve(), isFalse);
    });
  });
}
