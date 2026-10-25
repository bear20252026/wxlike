/// G13 页面测试的端口替身工厂。
///
/// 这里替换的是**尚未合入的上游绑定**（并行开发阶段），用来验证页面的三态、
/// 参数校验、幂等键复用与防重；它不证明真实域实现正确，最终集成验收必须换回
/// 真实端口（见 `integration-requests/G13`）。未指定的端口一律使用
/// `Unavailable*Port`，确保测试不会意外拿到静态数据。
library;

import 'package:wxlike_mobile/features/content/g13/core.dart';
import 'package:wxlike_mobile/features/content/g13/ports.dart';
import 'package:wxlike_mobile/features/content/g13/unavailable_ports.dart';

class TestFormatPort implements FormatPort {
  const TestFormatPort();

  @override
  String money(Money value) =>
      '${value.currency} ${(value.minor / 100).toStringAsFixed(2)}';
}

G13Ports testPorts({
  FormatPort? format,
  ContentPort? content,
  PublisherPort? publisher,
  VideoPort? video,
  LivePort? live,
  CallPort? calls,
  CommercePort? commerce,
  DiscoveryPort? discovery,
  MiniPort? mini,
  SearchPort? search,
  ProfilePort? profile,
  PreferencesPort? preferences,
  StatusPort? status,
  LocationPort? location,
  QrPort? qr,
}) {
  return G13Ports(
    format: format ?? const TestFormatPort(),
    content: content ?? const UnavailableContentPort(),
    publisher: publisher ?? const UnavailablePublisherPort(),
    video: video ?? const UnavailableVideoPort(),
    live: live ?? const UnavailableLivePort(),
    calls: calls ?? const UnavailableCallPort(),
    commerce: commerce ?? const UnavailableCommercePort(),
    discovery: discovery ?? const UnavailableDiscoveryPort(),
    mini: mini ?? const UnavailableMiniPort(),
    search: search ?? const UnavailableSearchPort(),
    profile: profile ?? const UnavailableProfilePort(),
    preferences: preferences ?? const UnavailablePreferencesPort(),
    status: status ?? const UnavailableStatusPort(),
    location: location ?? const UnavailableLocationPort(),
    qr: qr ?? const UnavailableQrPort(),
  );
}
