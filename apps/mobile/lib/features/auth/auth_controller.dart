/// 手机号登录 / 扫码授权的状态机。
///
/// 两个不变量：
/// 1. 倒计时来自服务端 deadline + 单调时钟；重启后重新问服务端，不能靠本地
///    计时器清零绕过重发限制。
/// 2. Dart 只提交"同意哪一版协议"和验证码；token 由 Rust 处理，这里不存。
library;

import 'dart:async';

import 'package:flutter/foundation.dart';

import '../../shell/contracts.dart';
import '../../src/service.dart';
import 'auth_models.dart';
import 'auth_ports.dart';

class AuthController extends ChangeNotifier {
  AuthController({
    required this.identity,
    required this.secrets,
    MonotonicClock? clock,
    DateTime Function()? nowUtc,
    this.locale = 'zh',
  })  : _clock = clock ?? StopwatchClock(),
        _nowUtc = nowUtc ?? (() => DateTime.now().toUtc());

  final IdentityPort identity;
  final AuthSecretStore secrets;
  final MonotonicClock _clock;
  final DateTime Function() _nowUtc;

  String locale;
  CountryCode country = defaultCountries.first;
  String phone = '';
  String code = '';

  PhoneChallenge? challenge;
  List<ConsentDocument> documents = const [];
  final Set<String> _agreed = {};

  bool loadingDocuments = false;
  bool sending = false;
  bool verifying = false;
  bool restoring = false;
  G12Failure? failure;

  Timer? _ticker;
  Duration _remainingAtReceipt = Duration.zero;
  Duration _receivedAt = Duration.zero;
  String? _verifyKey;

  String get normalizedPhone => phone.replaceAll(RegExp(r'[^0-9]'), '');

  bool get phoneValid => normalizedPhone.length >= 6 && normalizedPhone.length <= 15;

  bool get codeValid => RegExp(r'^\d{4,8}$').hasMatch(code);

  bool get agreementsComplete =>
      documents.where((d) => d.requiredToLogin).every((d) => _agreed.contains(d.id));

  Duration get remaining {
    if (challenge == null) return Duration.zero;
    final left = _remainingAtReceipt - (_clock.elapsed() - _receivedAt);
    return left.isNegative ? Duration.zero : left;
  }

  bool get canRequestCode =>
      !sending && phoneValid && (challenge == null || remaining == Duration.zero);

  bool get canVerify =>
      !verifying &&
      documents.isNotEmpty &&
      challenge != null &&
      challenge!.delivery == SmsDelivery.sms &&
      codeValid &&
      agreementsComplete &&
      remaining > Duration.zero;

  bool isAgreed(String documentId) => _agreed.contains(documentId);

  String get errorText {
    final f = failure;
    if (f == null) return '';
    return switch (f.code) {
      'CODE_INVALID' => '验证码不正确，请重试',
      'CODE_EXPIRED' => '验证码已过期，请重新获取',
      'SMS_UNAVAILABLE' => '短信服务暂不可用，请稍后再试',
      'RATE_LIMITED' => '发送太频繁，请稍后再试',
      'PHONE_INVALID' => '手机号格式不正确',
      'OAUTH_UNAUTHORIZED' => '外部授权未完成',
      _ => f.displayText,
    };
  }

  void setLocale(String next) {
    if (next == locale) return;
    locale = next;
    notifyListeners();
  }

  void setCountry(CountryCode next) {
    if (next.dialCode == country.dialCode) return;
    country = next;
    failure = null;
    notifyListeners();
  }

  void setPhone(String value) {
    phone = value;
    failure = null;
    notifyListeners();
  }

  void setCode(String value) {
    code = value.replaceAll(RegExp(r'[^0-9]'), '');
    failure = null;
    notifyListeners();
  }

  void toggleConsent(String documentId, bool agreed) {
    if (agreed) {
      _agreed.add(documentId);
    } else {
      _agreed.remove(documentId);
    }
    failure = null;
    notifyListeners();
  }

  Future<void> loadDocuments() async {
    if (loadingDocuments) return;
    loadingDocuments = true;
    failure = null;
    notifyListeners();
    try {
      documents = await identity.consentDocuments(locale: locale);
    } catch (error) {
      failure = asG12Failure(error, operation: '读取协议');
    } finally {
      loadingDocuments = false;
      notifyListeners();
    }
  }

  /// 进程重启后恢复服务端挑战；挑战已不存在或过期则清掉本地 id。
  Future<void> restore() async {
    if (restoring) return;
    restoring = true;
    notifyListeners();
    try {
      final id = await secrets.challengeId();
      if (id != null && id.isNotEmpty) {
        try {
          _adopt(await identity.challenge(id));
        } on G12Failure catch (error) {
          if (error.code == 'CODE_EXPIRED' || error.kind == G12FailureKind.invalidArgument) {
            await secrets.clearChallengeId();
          } else {
            failure = error;
          }
        } catch (error) {
          failure = asG12Failure(error, operation: '恢复登录挑战');
        }
      }
    } catch (error) {
      failure = asG12Failure(error, operation: '读取登录挑战');
    } finally {
      restoring = false;
      notifyListeners();
    }
  }

  Future<bool> requestCode({bool resend = false}) async {
    if (sending) return false;
    if (!phoneValid) {
      failure = const G12Failure(
        G12FailureKind.invalidArgument,
        'PHONE_INVALID',
        '手机号格式不正确',
      );
      notifyListeners();
      return false;
    }
    sending = true;
    failure = null;
    notifyListeners();
    try {
      final PhoneChallenge next;
      if (resend && challenge != null) {
        next = await identity.resendSmsCode(
          challenge!.id,
          idempotencyKey: newIdempotencyKey('sms-resend'),
        );
      } else {
        next = await identity.requestSmsCode(
          dialCode: country.dialCode,
          nationalNumber: normalizedPhone,
          idempotencyKey: newIdempotencyKey('sms-request'),
        );
      }
      if (next.delivery != SmsDelivery.sms) {
        failure = G12Failure(
          G12FailureKind.unavailable,
          'SMS_UNAVAILABLE',
          next.delivery == SmsDelivery.rateLimited ? '发送太频繁' : '短信通道不可用',
          retryable: true,
        );
      }
      _adopt(next);
      try {
        await secrets.saveChallengeId(next.id);
      } catch (error) {
        failure = asG12Failure(error, operation: '保存登录挑战');
      }
      return next.delivery == SmsDelivery.sms;
    } catch (error) {
      failure = asG12Failure(error, operation: '发送验证码');
      return false;
    } finally {
      sending = false;
      notifyListeners();
    }
  }

  /// 验证成功才返回会话；页面据此进入聊天，勾选协议本身不触发导航。
  Future<SessionInfo?> verify() async {
    if (verifying || !canVerify) return null;
    verifying = true;
    failure = null;
    notifyListeners();
    final key = _verifyKey ??= newIdempotencyKey('sms-verify');
    try {
      final nowMs = _nowUtc().millisecondsSinceEpoch;
      final consents = documents
          .where((d) => _agreed.contains(d.id))
          .map((d) => ConsentAcceptance(
                documentId: d.id,
                version: d.version,
                acceptedAtMs: nowMs,
              ))
          .toList();
      final session = await identity.verifySmsCode(
        challenge!.id,
        code,
        consents: consents,
        idempotencyKey: key,
      );
      _verifyKey = null;
      await secrets.clearChallengeId();
      challenge = null;
      code = '';
      return session;
    } catch (error) {
      final failure = asG12Failure(error, operation: '验证码登录');
      this.failure = failure;
      // 验证码错误/过期是终态，换码重试要新 key；网络类失败复用 key 防重复开户。
      if (failure.kind == G12FailureKind.invalidArgument) _verifyKey = null;
      return null;
    } finally {
      verifying = false;
      notifyListeners();
    }
  }

  void _adopt(PhoneChallenge next) {
    challenge = next;
    final now = _nowUtc();
    _remainingAtReceipt = next.resendAtUtc.difference(now);
    if (_remainingAtReceipt.isNegative) _remainingAtReceipt = Duration.zero;
    _receivedAt = _clock.elapsed();
    _startTicker();
  }

  void _startTicker() {
    _ticker?.cancel();
    if (remaining == Duration.zero) return;
    _ticker = Timer.periodic(const Duration(seconds: 1), (timer) {
      if (remaining == Duration.zero) {
        timer.cancel();
      }
      notifyListeners();
    });
  }

  @override
  void dispose() {
    _ticker?.cancel();
    super.dispose();
  }
}

class QrApprovalController extends ChangeNotifier {
  QrApprovalController({
    required this.identity,
    required this.challengeId,
    MonotonicClock? clock,
    DateTime Function()? nowUtc,
  })  : _clock = clock ?? StopwatchClock(),
        _nowUtc = nowUtc ?? (() => DateTime.now().toUtc());

  final IdentityPort identity;
  final String challengeId;
  final MonotonicClock _clock;
  final DateTime Function() _nowUtc;

  QrLoginChallenge? challenge;
  bool loading = false;
  bool deciding = false;
  bool closed = false;
  G12Failure? failure;

  Timer? _ticker;
  Duration _remainingAtReceipt = Duration.zero;
  Duration _receivedAt = Duration.zero;

  Duration get remaining {
    if (challenge == null) return Duration.zero;
    final left = _remainingAtReceipt - (_clock.elapsed() - _receivedAt);
    return left.isNegative ? Duration.zero : left;
  }

  bool get canDecide =>
      !closed && !deciding && challenge != null && challenge!.isPending && remaining > Duration.zero;

  Future<void> load() async {
    loading = true;
    failure = null;
    notifyListeners();
    try {
      _adopt(await identity.qrChallenge(challengeId));
    } catch (error) {
      failure = asG12Failure(error, operation: '读取扫码登录');
    } finally {
      loading = false;
      notifyListeners();
    }
  }

  Future<bool> approve({String idempotencyKey = ''}) async {
    if (!canDecide) return false;
    deciding = true;
    notifyListeners();
    try {
      _adopt(await identity.approveQr(
        challengeId,
        idempotencyKey: idempotencyKey.isEmpty
            ? newIdempotencyKey('qr-approve')
            : idempotencyKey,
      ));
      return true;
    } catch (error) {
      failure = asG12Failure(error, operation: '批准登录');
      return false;
    } finally {
      deciding = false;
      notifyListeners();
    }
  }

  Future<bool> reject() async {
    if (!canDecide) return false;
    deciding = true;
    notifyListeners();
    try {
      _adopt(await identity.rejectQr(
        challengeId,
        idempotencyKey: newIdempotencyKey('qr-reject'),
      ));
      return true;
    } catch (error) {
      failure = asG12Failure(error, operation: '拒绝登录');
      return false;
    } finally {
      deciding = false;
      notifyListeners();
    }
  }

  void _adopt(QrLoginChallenge next) {
    challenge = next;
    final now = _nowUtc();
    _remainingAtReceipt = next.expiresAtUtc.difference(now);
    if (_remainingAtReceipt.isNegative) _remainingAtReceipt = Duration.zero;
    _receivedAt = _clock.elapsed();
    _ticker?.cancel();
    if (remaining > Duration.zero) {
      _ticker = Timer.periodic(const Duration(seconds: 1), (timer) {
        if (closed || remaining == Duration.zero) timer.cancel();
        notifyListeners();
      });
    }
  }

  @override
  void dispose() {
    closed = true;
    _ticker?.cancel();
    super.dispose();
  }
}
