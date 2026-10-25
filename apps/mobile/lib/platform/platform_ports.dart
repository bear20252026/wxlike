/// 手机硬件与系统能力的唯一入口。
///
/// G13 以及后续页面不允许各自引插件；相机、相册、麦克风、定位、通知、
/// 文件、音频路由都从这里走。端口只有抽象，Android 实现见
/// `method_channel_platform.dart`，iOS 目标当前不在仓库内（见 README）。
library;

enum PlatformPermission { camera, microphone, photos, location, notifications, files }

enum PermissionStatus {
  granted,
  limited,
  denied,
  permanentlyDenied,
  restricted;

  bool get allowsUse => this == granted || this == limited;
}

enum MediaKind { image, video, file, audio }

class CapturedMedia {
  const CapturedMedia({
    required this.ref,
    required this.kind,
    this.name = '',
    this.mimeType = '',
    this.bytes = 0,
    this.durationMs = 0,
  });

  /// 平台侧可再次读取的引用（Android 为 content:// 或应用私有文件路径）。
  /// 它不是服务端 media_id；上传必须走 G04。
  final String ref;
  final MediaKind kind;
  final String name;
  final String mimeType;
  final int bytes;
  final int durationMs;
}

class LocationFix {
  const LocationFix({
    required this.latitude,
    required this.longitude,
    required this.accuracyMeters,
    required this.capturedAtMs,
  });

  final double latitude;
  final double longitude;
  final double accuracyMeters;
  final int capturedAtMs;
}

enum NetworkStatus { online, offline }

enum PlatformSettingsPage { app, notifications, location }

abstract class SecretStore {
  Future<String?> read(String key);

  Future<void> write(String key, String value);

  Future<void> delete(String key);
}

abstract class PlatformPort implements SecretStore {
  Future<PermissionStatus> permissionStatus(PlatformPermission permission);

  Future<PermissionStatus> requestPermission(PlatformPermission permission);

  Future<void> openSettings(PlatformSettingsPage page);

  /// 用户取消返回 null；权限被拒抛 [G12Failure]（见 `shell/contracts.dart`）。
  Future<CapturedMedia?> pickPhoto();

  Future<CapturedMedia?> takePhoto();

  Future<CapturedMedia?> pickFile();

  Future<void> startVoiceRecording();

  /// 取消录制时返回 null，并保证硬件已释放。
  Future<CapturedMedia?> stopVoiceRecording({required bool cancel});

  Future<void> playAudio(String ref);

  Future<void> stopAudio();

  Future<LocationFix> currentLocation();

  Future<void> setSpeaker(bool on);

  Future<void> saveImageToGallery(String ref);

  Stream<NetworkStatus> networkStatus();
}

class PushToken {
  const PushToken({required this.platform, required this.token});

  final String platform;
  final String token;
}

abstract class PushPort {
  /// 未配置 FCM/APNs 时返回 null（不是空字符串）。
  Future<PushToken?> token();

  /// 退出登录时撤销本机推送。当前服务端没有删除端点，未接线时明确抛错。
  Future<void> revoke();
}

enum AppLifecycleSignal { resumed, inactive, paused, detached }

abstract class LifecyclePort {
  Stream<AppLifecycleSignal> get signals;

  AppLifecycleSignal get current;
}
