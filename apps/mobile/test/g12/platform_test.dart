import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:wxlike_mobile/platform/method_channel_platform.dart';
import 'package:wxlike_mobile/platform/platform_ports.dart';
import 'package:wxlike_mobile/shell/contracts.dart';

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();
  final messenger =
      TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger;
  late List<MethodCall> calls;
  late MethodChannelPlatform platform;

  setUp(() {
    calls = [];
    platform = MethodChannelPlatform();
  });

  tearDown(() {
    messenger.setMockMethodCallHandler(platformChannel, null);
  });

  void handle(Future<Object?> Function(MethodCall call) handler) {
    messenger.setMockMethodCallHandler(platformChannel, (call) {
      calls.add(call);
      return handler(call);
    });
  }

  test('权限状态按宿主返回值映射', () async {
    handle(
      (call) async =>
          call.method == 'permission.status' ? 'granted' : 'permanentlyDenied',
    );
    expect(
      await platform.permissionStatus(PlatformPermission.camera),
      PermissionStatus.granted,
    );
    expect(
      await platform.requestPermission(PlatformPermission.camera),
      PermissionStatus.permanentlyDenied,
    );
    expect(calls.last.arguments, {'permission': 'camera'});
  });

  test('采集结果映射为 CapturedMedia，用户取消返回 null', () async {
    handle((call) async {
      if (call.method == 'photo.pick') {
        return {
          'ref': 'content://media/1',
          'kind': 'image',
          'name': 'a.jpg',
          'mime_type': 'image/jpeg',
          'bytes': 120,
          'duration_ms': 0,
        };
      }
      throw PlatformException(code: 'USER_CANCELED');
    });

    final media = await platform.pickPhoto();
    expect(media?.ref, 'content://media/1');
    expect(media?.kind, MediaKind.image);
    expect(media?.bytes, 120);
    expect(await platform.takePhoto(), isNull);
  });

  test('拒绝与超时保留可区分的失败类型', () async {
    handle((call) async {
      if (call.method == 'photo.capture') {
        throw PlatformException(code: 'PERMISSION_DENIED', message: '没有相机权限');
      }
      throw PlatformException(code: 'TIMEOUT', message: '定位超时');
    });

    await expectLater(
      platform.takePhoto(),
      throwsA(
        isA<G12Failure>().having(
          (f) => f.kind,
          'kind',
          G12FailureKind.forbidden,
        ),
      ),
    );
    await expectLater(
      platform.currentLocation(),
      throwsA(
        isA<G12Failure>().having((f) => f.kind, 'kind', G12FailureKind.timeout),
      ),
    );
  });

  test('通道缺失时明确报未接线，而不是静默成功', () async {
    messenger.setMockMethodCallHandler(platformChannel, null);

    await expectLater(
      platform.read('token'),
      throwsA(
        isA<G12Failure>().having((f) => f.code, 'code', 'PLATFORM_UNWIRED'),
      ),
    );
  });

  test('安全存储与推送 token 的参数形状固定', () async {
    handle((call) async {
      switch (call.method) {
        case 'secret.read':
          return 'pin-value';
        case 'secret.write':
        case 'secret.delete':
          return null;
        case 'push.token':
          return {'platform': 'android', 'token': 'fcm-token'};
        default:
          return null;
      }
    });

    expect(await platform.read('pin'), 'pin-value');
    await platform.write('pin', '1234');
    await platform.delete('pin');
    expect(calls[1].arguments, {'key': 'pin', 'value': '1234'});
    expect(calls[2].arguments, {'key': 'pin'});

    final push = MethodChannelPushPort();
    final token = await push.token();
    expect(token?.platform, 'android');
    expect(token?.token, 'fcm-token');
  });

  test('推送桥缺失时撤销也报未接线', () async {
    messenger.setMockMethodCallHandler(platformChannel, null);
    final push = MethodChannelPushPort();
    await expectLater(
      push.revoke(),
      throwsA(isA<G12Failure>().having((f) => f.code, 'code', 'PUSH_UNWIRED')),
    );
  });
}
