import 'service.dart';

/// B4 的 Android 侧骨架：拿到设备 token，把它交给 Rust 注册到服务端。
///
/// 为什么这里没有 `firebase_messaging`：加了它就要在构建期提供
/// `google-services.json` 与 Google Cloud 项目，`flutter build apk` 会直接失败，
/// B2 的可验证验收项就被 B4 的不可验证项拖住了。所以这一步做成一个接口 +
/// 一条待办：FCM 项目建好之后，把 [ManualTokenSource] 换成
/// `FirebaseMessaging.instance.getToken()` 的实现，其余代码不用动。
///
/// "token 该发给哪个 URL、按什么字段发"不在这儿——那是
/// `WxlikeClient::register_push_token`（`crates/ffi`）的事。
abstract class PushTokenSource {
  /// 设备推送 token；拿不到时返回 null（未授权、无 Google Play 服务等）。
  Future<String?> token();
}

/// 手工粘贴 token：真机验收时用它把链路先跑通
/// （`adb logcat | grep FirebaseInstanceId` 能抓到 FCM token）。
class ManualTokenSource implements PushTokenSource {
  const ManualTokenSource(this.value);

  final String? value;

  @override
  Future<String?> token() async => value;
}

/// 注册结果，用于把"没注册"和"注册失败"分开显示。
enum PushRegistration {
  /// 没有 token 可用（未装 Google Play、用户没授权）
  noToken,
  registered,
  failed,
}

/// 把当前设备的推送 token 报到服务端。
///
/// 每次登录成功后调用一次即可：服务端按 `(user, platform)` 唯一，
/// 重复注册替换旧 token（`crates/platform/src/devices.rs`）。
Future<PushRegistration> registerDevicePush(
  WxlikeService service,
  PushTokenSource source,
) async {
  final token = await source.token();
  if (token == null || token.trim().isEmpty) return PushRegistration.noToken;
  try {
    await service.registerPushToken('android', token);
    return PushRegistration.registered;
  } on WxlikeErrorException {
    return PushRegistration.failed;
  }
}
