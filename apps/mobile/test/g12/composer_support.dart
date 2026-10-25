import 'dart:async';

import 'package:wxlike_mobile/features/chat/composer/composer_ports.dart';
import 'package:wxlike_mobile/platform/platform_ports.dart';
import 'package:wxlike_mobile/src/service.dart';

class FakePlatform implements PlatformPort {
  final Map<PlatformPermission, PermissionStatus> statuses = {};
  final Map<PlatformPermission, PermissionStatus> requests = {};
  final Map<String, String> secrets = {};
  final List<String> calls = [];

  CapturedMedia? nextMedia;
  bool cancelVoiceReturnsNull = false;
  LocationFix fix = const LocationFix(
    latitude: 31.2,
    longitude: 121.5,
    accuracyMeters: 12,
    capturedAtMs: 1,
  );
  final _network = StreamController<NetworkStatus>.broadcast();

  void emitNetwork(NetworkStatus status) => _network.add(status);

  @override
  Future<PermissionStatus> permissionStatus(
    PlatformPermission permission,
  ) async => statuses[permission] ?? PermissionStatus.granted;

  @override
  Future<PermissionStatus> requestPermission(
    PlatformPermission permission,
  ) async => requests[permission] ?? PermissionStatus.granted;

  @override
  Future<void> openSettings(PlatformSettingsPage page) async =>
      calls.add('settings:${page.name}');

  @override
  Future<CapturedMedia?> pickPhoto() async {
    calls.add('pickPhoto');
    return nextMedia;
  }

  @override
  Future<CapturedMedia?> takePhoto() async {
    calls.add('takePhoto');
    return nextMedia;
  }

  @override
  Future<CapturedMedia?> pickFile() async {
    calls.add('pickFile');
    return nextMedia;
  }

  @override
  Future<void> startVoiceRecording() async => calls.add('voice.start');

  @override
  Future<CapturedMedia?> stopVoiceRecording({required bool cancel}) async {
    calls.add('voice.stop:${cancel ? 'cancel' : 'send'}');
    if (cancel || cancelVoiceReturnsNull) return null;
    return nextMedia;
  }

  @override
  Future<void> playAudio(String ref) async => calls.add('audio.play:$ref');

  @override
  Future<void> stopAudio() async => calls.add('audio.stop');

  @override
  Future<LocationFix> currentLocation() async => fix;

  @override
  Future<void> setSpeaker(bool on) async => calls.add('speaker:$on');

  @override
  Future<void> saveImageToGallery(String ref) async =>
      calls.add('gallery:$ref');

  @override
  Stream<NetworkStatus> networkStatus() => _network.stream;

  @override
  Future<String?> read(String key) async => secrets[key];

  @override
  Future<void> write(String key, String value) async => secrets[key] = value;

  @override
  Future<void> delete(String key) async => secrets.remove(key);
}

class FakeAttachments implements AttachmentPort {
  Object? uploadError;
  Object? sendError;
  int uploadCalls = 0;
  int sendCalls = 0;
  int voiceCalls = 0;
  final List<String> uploadKeys = [];
  final List<String> sendKeys = [];
  OutboxState sendState = OutboxState.sent;
  String sendErrorText = '';

  @override
  Future<UploadedMedia> upload(
    CapturedMedia media, {
    required String idempotencyKey,
  }) async {
    uploadCalls++;
    uploadKeys.add(idempotencyKey);
    if (uploadError != null) throw uploadError!;
    return UploadedMedia(mediaId: 'media-1', kind: media.kind);
  }

  @override
  Future<SendOutcome> sendMedia(
    String conv,
    UploadedMedia media, {
    required String idempotencyKey,
  }) async {
    sendCalls++;
    sendKeys.add(idempotencyKey);
    if (sendError != null) throw sendError!;
    return SendOutcome(
      taskId: 't1',
      state: sendState,
      msgId: 'm1',
      error: sendErrorText,
    );
  }

  @override
  Future<SendOutcome> sendVoice(
    String conv,
    UploadedMedia media, {
    required int durationMs,
    required String idempotencyKey,
  }) async {
    voiceCalls++;
    sendKeys.add(idempotencyKey);
    if (sendError != null) throw sendError!;
    return SendOutcome(
      taskId: 't1',
      state: sendState,
      msgId: 'm1',
      error: sendErrorText,
    );
  }

  @override
  Future<void> playVoice(String mediaId) async {}
}

class FakeRedPackets implements RedPacketComposerPort {
  RedPacketComposerState? state;
  Object? error;

  @override
  Future<RedPacketComposerState?> current(String conv) async {
    if (error != null) throw error!;
    return state;
  }
}

class FakeEntries implements ComposerEntryPort {
  final List<ComposerEntry> opened = [];

  @override
  Future<void> open(ComposerEntry entry, String conv) async {
    opened.add(entry);
  }
}

class FakeActions implements MessageActionsPort {
  final List<String> forwarded = [];
  final List<String> withdrawn = [];

  @override
  Future<void> forward(
    String messageId,
    String targetConv, {
    required String idempotencyKey,
  }) async {
    forwarded.add('$messageId->$targetConv');
  }

  @override
  Future<void> withdraw(
    String messageId, {
    required String idempotencyKey,
  }) async {
    withdrawn.add(messageId);
  }
}

class FakePush implements PushPort {
  PushToken? value;
  Object? error;
  int revokes = 0;

  @override
  Future<PushToken?> token() async => value;

  @override
  Future<void> revoke() async {
    revokes++;
    if (error != null) throw error!;
  }
}
