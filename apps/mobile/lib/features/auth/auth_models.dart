/// G12-06 的视图模型：国家码、协议版本、短信挑战与扫码授权。
///
/// 字段与 `identity.login.*` / `identity.qr_login.*` 契约一一对应。
/// 是否过期、还能不能重发由服务端 deadline 决定，本地只做单调时钟倒计时。
library;

class CountryCode {
  const CountryCode({required this.dialCode, required this.iso, required this.name});

  final String dialCode;
  final String iso;
  final String name;

  String get label => '+$dialCode';
}

class ConsentDocument {
  const ConsentDocument({
    required this.id,
    required this.title,
    required this.version,
    required this.requiredToLogin,
    this.url = '',
    this.body = '',
  });

  final String id;
  final String title;

  /// 协议版本必须随同意记录一起提交，事后才能审计"同意的是哪一版"。
  final String version;
  final bool requiredToLogin;
  final String url;
  final String body;
}

class ConsentAcceptance {
  const ConsentAcceptance({
    required this.documentId,
    required this.version,
    required this.acceptedAtMs,
  });

  final String documentId;
  final String version;
  final int acceptedAtMs;

  Map<String, Object> toJson() => {
        'document_id': documentId,
        'version': version,
        'accepted_at_ms': acceptedAtMs,
      };
}

enum SmsDelivery { sms, unavailable, rateLimited }

class PhoneChallenge {
  const PhoneChallenge({
    required this.id,
    required this.maskedPhone,
    required this.delivery,
    required this.resendAtUtc,
    required this.expiresAtUtc,
    this.attemptsLeft = 3,
  });

  final String id;
  final String maskedPhone;
  final SmsDelivery delivery;

  /// 服务端给出的最早可重发时间（UTC）。
  final DateTime resendAtUtc;
  final DateTime expiresAtUtc;
  final int attemptsLeft;
}

enum QrChallengeStatus { pending, approved, rejected, expired, consumed }

class QrLoginChallenge {
  const QrLoginChallenge({
    required this.id,
    required this.deviceName,
    required this.deviceKind,
    required this.location,
    required this.issuedAtUtc,
    required this.expiresAtUtc,
    required this.status,
    this.scannedBy = '',
  });

  final String id;
  final String deviceName;
  final String deviceKind;
  final String location;
  final DateTime issuedAtUtc;
  final DateTime expiresAtUtc;
  final QrChallengeStatus status;

  /// 当前登录用户在该挑战上的扫码结果，用于提示"只影响目标桌面"。
  final String scannedBy;

  bool get isPending => status == QrChallengeStatus.pending;
}
