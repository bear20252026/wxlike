/// Android/iOS 平台桥的 Dart 端。
///
/// 只使用 Flutter 自带的 MethodChannel/EventChannel：目前不引任何插件，
/// 原生实现（Kotlin/Swift）通过 `integration-requests/G12/G12-05.yaml` 交给
/// 宿主组；通道缺失时每个入口都返回明确的 unavailable，而不是静默成功。
///
/// 契约（method -> 返回）：
/// * `permission.status` / `permission.request` -> `granted|limited|denied|permanentlyDenied|restricted`
/// * `photo.pick` / `photo.capture` / `file.pick` -> `{ref,kind,name,mime_type,bytes,duration_ms}` 或 null
/// * `voice.start`、`voice.stop{cancel}` -> 同上（音频时长在 duration_ms）
/// * `audio.play{ref}`、`audio.stop`、`audio.route{on}`、`gallery.save{ref}`
/// * `location.current` -> `{latitude,longitude,accuracy_m,captured_at_ms}`
/// * `settings.open{page}`
/// * `secret.read{key}` / `secret.write{key,value}` / `secret.delete{key}`
/// * `push.token` -> `{platform,token}` 或 null；`push.revoke`
/// * `network.status` -> `online|offline`，事件通道 `wxlike/platform/network` 推同一字符串
/// * 事件通道 `wxlike/platform/links` 推深链字符串
library;

import 'dart:async';

import 'package:flutter/services.dart';
import 'package:flutter/widgets.dart';

import '../shell/contracts.dart';
import 'platform_ports.dart';

const MethodChannel platformChannel = MethodChannel('wxlike/platform');
const EventChannel networkChannel = EventChannel('wxlike/platform/network');
const EventChannel linkChannel = EventChannel('wxlike/platform/links');

class MethodChannelPlatform implements PlatformPort {
  MethodChannelPlatform({MethodChannel? channel, EventChannel? network, EventChannel? links})
      : _channel = channel ?? platformChannel,
        _network = network ?? networkChannel,
        _links = links ?? linkChannel,
        _lifecycle = WidgetsBindingLifecyclePort();

  final MethodChannel _channel;
  final EventChannel _network;
  final EventChannel _links;
  final WidgetsBindingLifecyclePort _lifecycle;

  Stream<Uri>? _linkStream;

  LifecyclePort get lifecycle => _lifecycle;

  @override
  Future<PermissionStatus> permissionStatus(PlatformPermission permission) =>
      _permission('permission.status', permission);

  @override
  Future<PermissionStatus> requestPermission(PlatformPermission permission) =>
      _permission('permission.request', permission);

  Future<PermissionStatus> _permission(String method, PlatformPermission permission) async {
    final raw = await _invoke<String>(method, {'permission': permission.name});
    return _statusFromWire(raw);
  }

  @override
  Future<void> openSettings(PlatformSettingsPage page) =>
      _invoke<void>('settings.open', {'page': page.name});

  @override
  Future<CapturedMedia?> pickPhoto() => _media('photo.pick');

  @override
  Future<CapturedMedia?> takePhoto() => _media('photo.capture');

  @override
  Future<CapturedMedia?> pickFile() => _media('file.pick');

  @override
  Future<void> startVoiceRecording() => _invoke<void>('voice.start');

  @override
  Future<CapturedMedia?> stopVoiceRecording({required bool cancel}) =>
      _media('voice.stop', {'cancel': cancel});

  @override
  Future<void> playAudio(String ref) => _invoke<void>('audio.play', {'ref': ref});

  @override
  Future<void> stopAudio() => _invoke<void>('audio.stop');

  @override
  Future<LocationFix> currentLocation() async {
    final map = await _invoke<Map<Object?, Object?>>('location.current');
    if (map == null) {
      throw const G12Failure(G12FailureKind.unavailable, 'LOCATION_UNAVAILABLE', '定位没有返回结果');
    }
    return LocationFix(
      latitude: (map['latitude'] as num).toDouble(),
      longitude: (map['longitude'] as num).toDouble(),
      accuracyMeters: (map['accuracy_m'] as num?)?.toDouble() ?? 0,
      capturedAtMs: (map['captured_at_ms'] as num?)?.toInt() ?? DateTime.now().millisecondsSinceEpoch,
    );
  }

  @override
  Future<void> setSpeaker(bool on) => _invoke<void>('audio.route', {'on': on});

  @override
  Future<void> saveImageToGallery(String ref) => _invoke<void>('gallery.save', {'ref': ref});

  @override
  Stream<NetworkStatus> networkStatus() {
    return _network
        .receiveBroadcastStream()
        .map((event) => event == 'online' ? NetworkStatus.online : NetworkStatus.offline)
        .handleError((Object error) {
      throw _mapError(error, '订阅网络状态');
    });
  }

  /// 深链由平台层投递；宿主必须在 Activity/SceneDelegate 里转发。
  Stream<Uri> deepLinks() {
    return _linkStream ??= _links
        .receiveBroadcastStream()
        .map((event) => Uri.parse(event as String))
        .handleError((Object error) {
      throw _mapError(error, '订阅深链');
    });
  }

  @override
  Future<String?> read(String key) => _invoke<String>('secret.read', {'key': key});

  @override
  Future<void> write(String key, String value) => _invoke<void>('secret.write', {'key': key, 'value': value});

  @override
  Future<void> delete(String key) => _invoke<void>('secret.delete', {'key': key});

  Future<CapturedMedia?> _media(String method, [Map<String, Object?> args = const {}]) async {
    final map = await _invoke<Map<Object?, Object?>>(method, args);
    if (map == null) return null;
    return CapturedMedia(
      ref: map['ref'] as String,
      kind: _kindFromWire(map['kind'] as String?),
      name: map['name'] as String? ?? '',
      mimeType: map['mime_type'] as String? ?? '',
      bytes: (map['bytes'] as num?)?.toInt() ?? 0,
      durationMs: (map['duration_ms'] as num?)?.toInt() ?? 0,
    );
  }

  Future<T?> _invoke<T>(String method, [Map<String, Object?> args = const {}]) async {
    try {
      return await _channel.invokeMethod<T>(method, args);
    } on MissingPluginException {
      throw G12Failure(
        G12FailureKind.unavailable,
        'PLATFORM_UNWIRED',
        '平台桥 $method 尚未安装（见 G12-05 宿主接入请求）',
      );
    } on PlatformException catch (error) {
      if (error.code == 'USER_CANCELED') return null;
      throw _mapError(error, method);
    }
  }

  G12Failure _mapError(Object error, String operation) {
    if (error is G12Failure) return error;
    if (error is MissingPluginException) {
      return G12Failure(
        G12FailureKind.unavailable,
        'PLATFORM_UNWIRED',
        '平台桥 $operation 尚未安装（见 G12-05 宿主接入请求）',
      );
    }
    if (error is PlatformException) {
      final message = error.message ?? operation;
      return switch (error.code) {
        'PERMISSION_DENIED' => G12Failure(G12FailureKind.forbidden, error.code, message),
        'PERMISSION_PERMANENT' => G12Failure(G12FailureKind.forbidden, 'PERMISSION_DENIED', message),
        'UNAVAILABLE' || 'NO_HANDLER' => G12Failure(G12FailureKind.unavailable, error.code, message),
        'TIMEOUT' => G12Failure(G12FailureKind.timeout, error.code, message, retryable: true),
        _ => G12Failure(G12FailureKind.unexpected, error.code, message, retryable: true),
      };
    }
    return asG12Failure(error, operation: operation);
  }

  static PermissionStatus _statusFromWire(String? value) => switch (value) {
        'granted' => PermissionStatus.granted,
        'limited' => PermissionStatus.limited,
        'denied' => PermissionStatus.denied,
        'permanentlyDenied' || 'permanently_denied' => PermissionStatus.permanentlyDenied,
        'restricted' => PermissionStatus.restricted,
        _ => PermissionStatus.denied,
      };

  static MediaKind _kindFromWire(String? value) => switch (value) {
        'image' => MediaKind.image,
        'video' => MediaKind.video,
        'audio' => MediaKind.audio,
        _ => MediaKind.file,
      };
}

/// 前后台信号：直接来自 WidgetsBinding，不额外引插件。
class WidgetsBindingLifecyclePort implements LifecyclePort {
  WidgetsBindingLifecyclePort() {
    WidgetsBinding.instance.addObserver(_observer);
    _current = _fromState(WidgetsBinding.instance.lifecycleState);
  }

  final _controller = StreamController<AppLifecycleSignal>.broadcast();
  late final _Observer _observer = _Observer((signal) {
    _current = signal;
    _controller.add(signal);
  });
  AppLifecycleSignal _current = AppLifecycleSignal.resumed;

  @override
  Stream<AppLifecycleSignal> get signals => _controller.stream;

  @override
  AppLifecycleSignal get current => _current;

  void dispose() {
    WidgetsBinding.instance.removeObserver(_observer);
    _controller.close();
  }

  static AppLifecycleSignal _fromState(AppLifecycleState? state) => switch (state) {
        AppLifecycleState.resumed => AppLifecycleSignal.resumed,
        AppLifecycleState.inactive => AppLifecycleSignal.inactive,
        AppLifecycleState.paused || AppLifecycleState.hidden => AppLifecycleSignal.paused,
        AppLifecycleState.detached => AppLifecycleSignal.detached,
        null => AppLifecycleSignal.resumed,
      };
}

class _Observer with WidgetsBindingObserver {
  _Observer(this._onSignal);

  final ValueChanged<AppLifecycleSignal> _onSignal;

  @override
  void didChangeAppLifecycleState(AppLifecycleState state) {
    _onSignal(WidgetsBindingLifecyclePort._fromState(state));
  }
}

/// 推送 token：FCM/APNs 的获取与撤销都走宿主，Dart 只转交。
class MethodChannelPushPort implements PushPort {
  MethodChannelPushPort({MethodChannel? channel}) : _channel = channel ?? platformChannel;

  final MethodChannel _channel;

  @override
  Future<PushToken?> token() async {
    try {
      final map = await _channel.invokeMethod<Map<Object?, Object?>>('push.token');
      if (map == null) return null;
      final platform = map['platform'] as String? ?? '';
      final token = map['token'] as String? ?? '';
      if (platform.isEmpty || token.isEmpty) return null;
      return PushToken(platform: platform, token: token);
    } on MissingPluginException {
      throw const G12Failure(
        G12FailureKind.unavailable,
        'PUSH_UNWIRED',
        '推送桥尚未安装（APNs/FCM 需要宿主配置）',
      );
    } on PlatformException catch (error) {
      throw G12Failure(G12FailureKind.unavailable, error.code, error.message ?? '读取推送 token 失败');
    }
  }

  @override
  Future<void> revoke() async {
    try {
      await _channel.invokeMethod<void>('push.revoke');
    } on MissingPluginException {
      throw const G12Failure(
        G12FailureKind.unavailable,
        'PUSH_UNWIRED',
        '推送桥尚未安装（无法撤销本机推送）',
      );
    }
  }
}
