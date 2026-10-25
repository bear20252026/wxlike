/// 登录相关端口。
///
/// `identity.*` 由 G02 提供；在本轮绑定生成前，移动端只允许通过
/// [UnavailableIdentityPort] 明确报错，绝不用本地假验证码冒充登录成功。
/// 账号密码那条线已经真实存在（`WxlikeClient.login/register`），由
/// [ServiceAccountPort] 原样转接。
library;

import '../../platform/platform_ports.dart';
import '../../shell/contracts.dart';
import '../../src/service.dart';
import 'auth_models.dart';

const List<CountryCode> defaultCountries = [
  CountryCode(dialCode: '86', iso: 'CN', name: '中国大陆'),
  CountryCode(dialCode: '852', iso: 'HK', name: '中国香港'),
  CountryCode(dialCode: '853', iso: 'MO', name: '中国澳门'),
  CountryCode(dialCode: '886', iso: 'TW', name: '中国台湾'),
  CountryCode(dialCode: '1', iso: 'US', name: '美国/加拿大'),
  CountryCode(dialCode: '81', iso: 'JP', name: '日本'),
  CountryCode(dialCode: '82', iso: 'KR', name: '韩国'),
  CountryCode(dialCode: '65', iso: 'SG', name: '新加坡'),
  CountryCode(dialCode: '44', iso: 'GB', name: '英国'),
  CountryCode(dialCode: '49', iso: 'DE', name: '德国'),
];

abstract class IdentityPort {
  Future<List<ConsentDocument>> consentDocuments({String locale = 'zh'});

  Future<PhoneChallenge> requestSmsCode({
    required String dialCode,
    required String nationalNumber,
    required String idempotencyKey,
  });

  Future<PhoneChallenge> resendSmsCode(
    String challengeId, {
    required String idempotencyKey,
  });

  /// 恢复已有挑战（进程重启、返回登录页）。找不到或已过期时抛
  /// `CODE_EXPIRED`，页面据此清掉本地记录。
  Future<PhoneChallenge> challenge(String challengeId);

  Future<SessionInfo> verifySmsCode(
    String challengeId,
    String code, {
    required List<ConsentAcceptance> consents,
    required String idempotencyKey,
  });

  Future<QrLoginChallenge> qrChallenge(String challengeId);

  Future<QrLoginChallenge> approveQr(
    String challengeId, {
    required String idempotencyKey,
  });

  Future<QrLoginChallenge> rejectQr(
    String challengeId, {
    required String idempotencyKey,
  });
}

/// 绑定里已经存在的账号会话能力。
abstract class AccountPort {
  SessionInfo session();

  Future<SessionInfo> login(String name, String password);

  Future<SessionInfo> register(String name, String password);

  Future<void> logout();

  Future<LiveInfo> connect();

  Future<LiveInfo> liveInfo();

  Future<void> registerPushToken(String platform, String token);
}

class ServiceAccountPort implements AccountPort {
  ServiceAccountPort(this._service);

  final WxlikeService _service;

  @override
  SessionInfo session() => _service.session();

  @override
  Future<SessionInfo> login(String name, String password) => _service.login(name, password);

  @override
  Future<SessionInfo> register(String name, String password) => _service.register(name, password);

  @override
  Future<void> logout() => _service.logout();

  @override
  Future<LiveInfo> connect() => _service.connect();

  @override
  Future<LiveInfo> liveInfo() => _service.liveInfo();

  @override
  Future<void> registerPushToken(String platform, String token) =>
      _service.registerPushToken(platform, token);
}

/// 上游尚未接线时的行为：每个入口都失败，页面显示阻塞原因。
class UnavailableIdentityPort implements IdentityPort {
  const UnavailableIdentityPort();

  Never _notWired(String method) =>
      throw G12Failure.upstreamNotWired(method, 'identity.$method 尚未接入 G02 绑定');

  @override
  Future<List<ConsentDocument>> consentDocuments({String locale = 'zh'}) =>
      _notWired('consent_documents');

  @override
  Future<PhoneChallenge> requestSmsCode({
    required String dialCode,
    required String nationalNumber,
    required String idempotencyKey,
  }) =>
      _notWired('login.request_code');

  @override
  Future<PhoneChallenge> resendSmsCode(String challengeId, {required String idempotencyKey}) =>
      _notWired('login.resend_code');

  @override
  Future<PhoneChallenge> challenge(String challengeId) => _notWired('login.challenge');

  @override
  Future<SessionInfo> verifySmsCode(
    String challengeId,
    String code, {
    required List<ConsentAcceptance> consents,
    required String idempotencyKey,
  }) =>
      _notWired('login.verify_code');

  @override
  Future<QrLoginChallenge> qrChallenge(String challengeId) => _notWired('qr_login.challenge');

  @override
  Future<QrLoginChallenge> approveQr(String challengeId, {required String idempotencyKey}) =>
      _notWired('qr_login.approve');

  @override
  Future<QrLoginChallenge> rejectQr(String challengeId, {required String idempotencyKey}) =>
      _notWired('qr_login.reject');
}

/// 登录挑战 id 的最小持久化：只存挑战 id，不存手机号、验证码或 token。
const String loginChallengeSecretKey = 'g12.login.challenge_id';

class AuthSecretStore {
  const AuthSecretStore(this._secrets);

  final SecretStore _secrets;

  Future<String?> challengeId() => _secrets.read(loginChallengeSecretKey);

  Future<void> saveChallengeId(String id) => _secrets.write(loginChallengeSecretKey, id);

  Future<void> clearChallengeId() => _secrets.delete(loginChallengeSecretKey);
}
