import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:wxlike_mobile/features/auth/auth_models.dart';
import 'package:wxlike_mobile/features/auth/auth_ports.dart';
import 'package:wxlike_mobile/features/auth/qr_approval/qr_approval_page.dart';
import 'package:wxlike_mobile/shell/contracts.dart';
import 'package:wxlike_mobile/shell/theme.dart';
import 'package:wxlike_mobile/src/service.dart';

class QrIdentity implements IdentityPort {
  QrIdentity(this.current);

  QrLoginChallenge? current;
  int approves = 0;
  int rejects = 0;
  int scans = 0;
  String? lastScanSecret;
  Object? error;

  @override
  Future<QrLoginChallenge> qrChallenge(String challengeId) async {
    if (error != null) throw error!;
    return current!;
  }

  @override
  Future<QrLoginChallenge> scanQr(String challengeId, {required String secret}) async {
    scans++;
    lastScanSecret = secret;
    if (error != null) throw error!;
    return current = QrLoginChallenge(
      id: current!.id,
      deviceName: current!.deviceName,
      deviceKind: current!.deviceKind,
      location: current!.location,
      issuedAtUtc: current!.issuedAtUtc,
      expiresAtUtc: current!.expiresAtUtc,
      status: QrChallengeStatus.scanned,
      scannedBy: 'me',
    );
  }

  @override
  Future<QrLoginChallenge> approveQr(String challengeId, {required String idempotencyKey}) async {
    approves++;
    return current = QrLoginChallenge(
      id: current!.id,
      deviceName: current!.deviceName,
      deviceKind: current!.deviceKind,
      location: current!.location,
      issuedAtUtc: current!.issuedAtUtc,
      expiresAtUtc: current!.expiresAtUtc,
      status: QrChallengeStatus.approved,
      scannedBy: 'me',
    );
  }

  @override
  Future<QrLoginChallenge> rejectQr(String challengeId, {required String idempotencyKey}) async {
    rejects++;
    return current = QrLoginChallenge(
      id: current!.id,
      deviceName: current!.deviceName,
      deviceKind: current!.deviceKind,
      location: current!.location,
      issuedAtUtc: current!.issuedAtUtc,
      expiresAtUtc: current!.expiresAtUtc,
      status: QrChallengeStatus.rejected,
    );
  }

  @override
  Future<List<ConsentDocument>> consentDocuments({String locale = 'zh'}) => throw UnimplementedError();

  @override
  Future<PhoneChallenge> requestSmsCode({
    required String dialCode,
    required String nationalNumber,
    required String idempotencyKey,
  }) =>
      throw UnimplementedError();

  @override
  Future<PhoneChallenge> resendSmsCode(String challengeId, {required String idempotencyKey}) =>
      throw UnimplementedError();

  @override
  Future<PhoneChallenge> challenge(String challengeId) => throw UnimplementedError();

  @override
  Future<SessionInfo> verifySmsCode(
    String challengeId,
    String code, {
    required List<ConsentAcceptance> consents,
    required String idempotencyKey,
  }) =>
      throw UnimplementedError();
}

QrLoginChallenge challenge() {
  final now = DateTime.now().toUtc();
  return QrLoginChallenge(
    id: 'qr-1',
    deviceName: 'MacBook Pro',
    deviceKind: 'desktop',
    location: '上海',
    issuedAtUtc: now,
    expiresAtUtc: now.add(const Duration(minutes: 2)),
    status: QrChallengeStatus.pending,
  );
}

void main() {
  testWidgets('扫码确认页展示目标设备，批准只影响该挑战', (tester) async {
    final identity = QrIdentity(challenge());
    await tester.pumpWidget(MaterialApp(
      theme: WxTheme.light(),
      home: QrApprovalPage(
        identity: identity,
        challengeId: 'qr-1',
        session: const SessionInfo(userId: 'u1', server: 's', signedIn: true),
      ),
    ));
    await tester.pumpAndSettle();

    expect(find.text('MacBook Pro'), findsOneWidget);
    expect(find.text('仅目标桌面设备'), findsOneWidget);
    expect(find.text('qr-1'), findsOneWidget);

    await tester.tap(find.byKey(const Key('qr-approve')));
    await tester.pumpAndSettle();
    expect(find.textContaining('仅批准这一台桌面'), findsOneWidget);

    await tester.tap(find.descendant(of: find.byType(AlertDialog), matching: find.text('批准登录')));
    await tester.pumpAndSettle();
    expect(identity.approves, 1);
    // 成功显示为契约状态，而不是只闪过一条 toast。
    expect(find.text('APPROVED'), findsOneWidget);
    expect(find.textContaining('已批准，桌面端将自动登录'), findsOneWidget);
    expect(find.byKey(const Key('qr-state')), findsOneWidget);

    final button = tester.widget<FilledButton>(find.byKey(const Key('qr-approve')));
    expect(button.onPressed, isNull, reason: '终态后不能再次批准');
  });

  testWidgets('挑战过期时按钮不可用，并说明原因', (tester) async {
    final expired = QrLoginChallenge(
      id: 'qr-2',
      deviceName: 'PC',
      deviceKind: 'desktop',
      location: '',
      issuedAtUtc: DateTime.now().toUtc().subtract(const Duration(minutes: 5)),
      expiresAtUtc: DateTime.now().toUtc().subtract(const Duration(minutes: 1)),
      status: QrChallengeStatus.pending,
    );
    final identity = QrIdentity(expired);
    await tester.pumpWidget(MaterialApp(
      theme: WxTheme.light(),
      home: QrApprovalPage(identity: identity, challengeId: 'qr-2'),
    ));
    await tester.pumpAndSettle();

    expect(find.textContaining('挑战已过期'), findsOneWidget);
    final button = tester.widget<FilledButton>(find.byKey(const Key('qr-approve')));
    expect(button.onPressed, isNull);
    expect(identity.approves, 0);
  });

  testWidgets('读取挑战失败时显示阻塞与重试', (tester) async {
    final identity = QrIdentity(null)
      ..error = const G12Failure(G12FailureKind.unavailable, 'UPSTREAM_NOT_WIRED', '身份域未接线');
    await tester.pumpWidget(MaterialApp(
      theme: WxTheme.light(),
      home: QrApprovalPage(identity: identity, challengeId: 'qr-3'),
    ));
    await tester.pumpAndSettle();

    expect(find.text('无法读取登录挑战'), findsOneWidget);
    expect(find.textContaining('身份域未接线'), findsOneWidget);
    expect(find.text('重新检查'), findsOneWidget);
  });
}
