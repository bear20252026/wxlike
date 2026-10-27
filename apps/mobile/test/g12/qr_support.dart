import 'dart:async';

import 'package:wxlike_mobile/features/auth/auth_models.dart';
import 'package:wxlike_mobile/features/auth/auth_ports.dart';
import 'package:wxlike_mobile/features/auth/qr_approval/qr_transport.dart';
import 'package:wxlike_mobile/features/chat/timeline/messaging_ports.dart';
import 'package:wxlike_mobile/shell/contracts.dart';
import 'package:wxlike_mobile/src/service.dart';

/// 记录型传输：把每一次请求的形状原样留下来。
///
/// 它存在的意义就是让"假绿"无处藏身——断言打在 `requests` 的 **方法与路径与
/// body** 上，而不是打在"调用过某个方法"上。`logLines` 模拟日志出口：传输层
/// 只允许打印 `request.toString()`（方法 + 路径），所以 secret 一旦漏进日志，
/// 这里的断言就会红。
class RecordingQrTransport implements QrApiTransport {
  RecordingQrTransport({this.reply});

  final List<QrApiRequest> requests = [];
  final List<String> logLines = [];
  final List<String> bodies = [];

  /// 按请求决定回包；不给就是 200 `{state: SCANNED}`。
  final QrApiResponse Function(QrApiRequest request)? reply;

  bool disposed = false;

  @override
  Future<QrApiResponse> send(QrApiRequest request) async {
    // 这就是生产实现里 `debugPrint('$request')` 的等价物：只有方法与路径。
    logLines.add('$request');
    requests.add(request);
    // body 单独留一份"如果谁把它打印了会怎样"的证据，供断言它没进 logLines。
    if (request.body != null) bodies.add(request.body.toString());
    return reply?.call(request) ??
        const QrApiResponse(status: 200, body: {'state': 'SCANNED'});
  }

  @override
  void dispose() => disposed = true;

  Iterable<String> get paths => requests.map((r) => '${r.method} ${r.path}');

  Map<String, Object?>? bodyOf(String pathPart) {
    for (final request in requests) {
      if (request.path.endsWith(pathPart)) return request.body;
    }
    return null;
  }
}

/// 会话端口替身：只够把壳推进到"已登录"，不参与扫码链路。
class StubAccount implements AccountPort {
  StubAccount([this._session = const SessionInfo(userId: 'u1', server: 'http://srv', signedIn: true)]);

  SessionInfo _session;

  @override
  SessionInfo session() => _session;

  @override
  Future<SessionInfo> login(String name, String password) async => _session;

  @override
  Future<SessionInfo> register(String name, String password) async => _session;

  @override
  Future<void> logout() async {
    _session = const SessionInfo(userId: '', server: '', signedIn: false);
  }

  @override
  Future<LiveInfo> connect() async => const LiveInfo(state: LiveState.online, userId: 'u1', server: 'http://srv', pending: 0, pumpError: '');

  @override
  Future<LiveInfo> liveInfo() async => const LiveInfo(state: LiveState.online, userId: 'u1', server: 'http://srv', pending: 0, pumpError: '');

  @override
  Future<void> registerPushToken(String platform, String token) async {}
}

/// 消息端口替身：壳只要求 `watch` 不抛错。
class StubMessaging implements MessagingPort {
  final _events = StreamController<ChatEvent>.broadcast();

  void close() => _events.close();

  @override
  SessionInfo session() => const SessionInfo(userId: 'u1', server: 'http://srv', signedIn: true);

  @override
  Future<ConversationList> conversations() async =>
      const ConversationList(conversations: [], fromCache: false);

  @override
  Future<LiveInfo> liveInfo() async => const LiveInfo(state: LiveState.online, userId: 'u1', server: 'http://srv', pending: 0, pumpError: '');

  @override
  Future<MessagePage> history(String conv, int beforeSeq, int limit) async =>
      const MessagePage(messages: [], fromCache: false, nextBeforeSeq: -1);

  @override
  Future<UnreadState> unread(String conv) async =>
      const UnreadState(conv: 'c1', owner: 'direct', headSeq: 0, readSeq: 0, unread: 0);

  @override
  Future<UnreadState> markRead(String conv, int uptoSeq) async => unread(conv);

  @override
  Future<SendOutcome> sendText(String conv, String text) async =>
      const SendOutcome(taskId: 't1', state: OutboxState.sent, msgId: 'm1', error: '');

  @override
  Future<OutboxSummary> outboxStatus() async => const OutboxSummary(pending: 0, failed: 0, entries: []);

  @override
  Future<FlushReport> flushOutbox() async => const FlushReport(sent: 0, retrying: 0, failed: 0);

  @override
  Future<OutboxSummary> retryOutbox(String taskId) async =>
      const OutboxSummary(pending: 0, failed: 0, entries: []);

  @override
  Future<bool> dropOutbox(String taskId) async => true;

  @override
  Future<LiveInfo> connect() async => liveInfo();

  @override
  Stream<ChatEvent> watch(String conv, {Duration interval = const Duration(seconds: 2)}) =>
      _events.stream;
}

/// 取"必然抛出的 G12Failure"。没抛就直接红——不允许写成 `try/catch {}` 那种
/// 空转断言（本轮之前的教训）。
Future<G12Failure> guard(Future<Object?> Function() body) async {
  try {
    await body();
  } on G12Failure catch (failure) {
    return failure;
  }
  throw StateError('期望抛 G12Failure，结果却成功了');
}

/// 载荷常量：形状照服务端来——票号 12 字节 hex，secret 24 字节 hex。
const String qrTicketId = 'a1b2c3d4e5f60718293a4b5c';
const String qrSecret = '0123456789abcdef0123456789abcdef0123456789abcdef';
const String qrPayload = 'wxlike-qr://$qrTicketId/$qrSecret';

/// 把 `identity.scanQr/approveQr` 走通后该出现的两次请求。
List<String> expectedQrCalls(String ticketId) => [
      'POST /api/qr/tickets/$ticketId/scan',
      'POST /api/qr/tickets/$ticketId/approve',
    ];

/// 端口替身：不参与本轮，只要求"调用即报错"。
class UnusedFallback implements IdentityPort {
  const UnusedFallback();

  Never _no() => throw UnimplementedError('本卡不接这条线');

  @override
  Future<List<ConsentDocument>> consentDocuments({String locale = 'zh'}) => _no();

  @override
  Future<PhoneChallenge> requestSmsCode({
    required String dialCode,
    required String nationalNumber,
    required String idempotencyKey,
  }) =>
      _no();

  @override
  Future<PhoneChallenge> resendSmsCode(String challengeId, {required String idempotencyKey}) => _no();

  @override
  Future<PhoneChallenge> challenge(String challengeId) => _no();

  @override
  Future<SessionInfo> verifySmsCode(
    String challengeId,
    String code, {
    required List<ConsentAcceptance> consents,
    required String idempotencyKey,
  }) =>
      _no();

  @override
  Future<QrLoginChallenge> qrChallenge(String challengeId) => _no();

  @override
  Future<QrLoginChallenge> scanQr(String challengeId, {required String secret}) => _no();

  @override
  Future<QrLoginChallenge> approveQr(String challengeId, {required String idempotencyKey}) => _no();

  @override
  Future<QrLoginChallenge> rejectQr(String challengeId, {required String idempotencyKey}) => _no();
}
