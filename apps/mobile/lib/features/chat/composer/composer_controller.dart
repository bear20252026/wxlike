/// 输入器状态机：文字、语音按住录制、附件上传/发送、红包入口。
///
/// 三种失败必须分开显示：采集/权限、上传、发送。上传失败可以单独重试；
/// 离线时保留待上传媒体，网络恢复后重试。红包金额只接受 G06 返回的状态。
library;

import 'dart:async';

import 'package:flutter/foundation.dart';

import '../../../platform/platform_ports.dart';
import '../../../shell/contracts.dart';
import '../../../src/service.dart';
import 'composer_ports.dart';
import 'draft_store.dart';

enum ComposerStage { idle, capturing, uploading, sending }

enum ComposerFailureStage { permission, capture, upload, send, voice, unavailable, redPacket }

class ComposerController extends ChangeNotifier {
  ComposerController({
    required this.conv,
    required this.drafts,
    required this.platform,
    required this.attachments,
    required this.redPackets,
    required this.entries,
    required this.actions,
    required this.sendTextHandler,
    required this.onSent,
    this.onOpenRedPacket,
  }) : text = drafts.read(conv) {
    final restored = drafts.attachment(conv);
    if (restored != null) {
      _media = restored.toMedia();
      _uploaded = restored.toUploaded();
      failureStage = _uploaded == null ? ComposerFailureStage.upload : ComposerFailureStage.send;
      failure = _uploaded == null ? '上次的附件还没上传完成，可以重试' : '上次的附件还没发送成功，可以重试';
    }
  }

  final String conv;
  final DraftStore drafts;
  final PlatformPort platform;
  final AttachmentPort attachments;
  final RedPacketComposerPort redPackets;
  final ComposerEntryPort entries;
  final MessageActionsPort actions;
  final Future<SendOutcome?> Function(String text) sendTextHandler;
  final Future<void> Function() onSent;
  final ValueChanged<RedPacketComposerState>? onOpenRedPacket;

  String text;
  bool voiceMode = false;
  bool panelOpen = false;
  bool recording = false;
  bool cancelRecording = false;
  ChatMessage? quoted;
  ComposerStage stage = ComposerStage.idle;
  ComposerFailureStage? failureStage;
  String? failure;
  RedPacketComposerState? redPacket;

  CapturedMedia? _media;
  UploadedMedia? _uploaded;
  int _voiceDurationMs = 0;
  String? _uploadKey;
  String? _sendKey;
  Stopwatch? _recordStarted;
  bool _disposed = false;

  bool get busy => stage != ComposerStage.idle;

  bool get canSend => text.trim().isNotEmpty && !busy;

  void updateText(String value) {
    text = value;
    drafts.write(conv, value);
    if (failureStage == ComposerFailureStage.send) {
      failure = null;
      failureStage = null;
    }
    _notify();
  }

  void setVoiceMode(bool enabled) {
    voiceMode = enabled;
    panelOpen = false;
    _notify();
  }

  void togglePanel() {
    panelOpen = !panelOpen;
    voiceMode = false;
    _notify();
  }

  void quote(ChatMessage message) {
    quoted = message;
    _notify();
  }

  void clearQuote() {
    quoted = null;
    _notify();
  }

  Future<void> sendText() async {
    if (!canSend) return;
    if (quoted != null) {
      // 结构化引用需要 messaging.send_quote（G03）；现在只发纯文本会把引用悄悄丢掉。
      _fail(ComposerFailureStage.unavailable, '引用发送需 G03 结构化消息，当前未接线');
      return;
    }
    final body = text.trim();
    stage = ComposerStage.sending;
    failure = null;
    failureStage = null;
    _notify();
    try {
      final outcome = await sendTextHandler(body);
      if (_disposed) return;
      if (outcome == null) {
        _fail(ComposerFailureStage.send, '发送失败，请检查连接后重试');
        return;
      }
      if (outcome.state == OutboxState.failed) {
        _fail(
          ComposerFailureStage.send,
          outcome.error.isEmpty ? '发送失败' : '发送失败：${outcome.error}',
        );
        return;
      }
      text = '';
      drafts.clear(conv);
      quoted = null;
      await onSent();
    } catch (error) {
      if (!_disposed) _fail(ComposerFailureStage.send, asG12Failure(error).displayText);
    } finally {
      stage = ComposerStage.idle;
      _notify();
    }
  }

  Future<void> pickAlbum() => _capture(
        PlatformPermission.photos,
        () => platform.pickPhoto(),
      );

  Future<void> takePhoto() => _capture(
        PlatformPermission.camera,
        () => platform.takePhoto(),
      );

  Future<void> pickFile() => _capture(
        PlatformPermission.files,
        () => platform.pickFile(),
      );

  Future<void> _capture(
    PlatformPermission permission,
    Future<CapturedMedia?> Function() capture,
  ) async {
    if (busy) return;
    stage = ComposerStage.capturing;
    failure = null;
    failureStage = null;
    panelOpen = false;
    _notify();
    try {
      if (!await _ensurePermission(permission)) return;
      final media = await capture();
      if (_disposed) return;
      if (media == null) return; // 用户取消：不是失败，也不假成功。
      _media = media;
      drafts.putAttachment(conv, AttachmentDraft.fromMedia(media));
      _uploadKey = newIdempotencyKey('media-upload');
      _sendKey = newIdempotencyKey('media-send');
      await _uploadAndSend();
    } catch (error) {
      if (!_disposed) _fail(ComposerFailureStage.capture, _captureMessage(error));
    } finally {
      if (!recording) stage = ComposerStage.idle;
      _notify();
    }
  }

  Future<void> startVoiceRecording() async {
    if (busy || recording) return;
    failure = null;
    failureStage = null;
    _notify();
    try {
      if (!await _ensurePermission(PlatformPermission.microphone)) return;
      await platform.startVoiceRecording();
      if (_disposed) {
        await platform.stopVoiceRecording(cancel: true);
        return;
      }
      recording = true;
      cancelRecording = false;
      _recordStarted = Stopwatch()..start();
      stage = ComposerStage.capturing;
    } catch (error) {
      recording = false;
      stage = ComposerStage.idle;
      _fail(ComposerFailureStage.voice, '无法开始录音：${asG12Failure(error).displayText}');
    }
    _notify();
  }

  void updateVoiceDrag(double dy) {
    if (!recording) return;
    final next = dy < -70;
    if (next != cancelRecording) {
      cancelRecording = next;
      _notify();
    }
  }

  Future<void> finishVoiceRecording() async {
    if (!recording) return;
    final cancel = cancelRecording;
    recording = false;
    cancelRecording = false;
    _voiceDurationMs = _recordStarted?.elapsedMilliseconds ?? 0;
    _notify();
    try {
      final media = await platform.stopVoiceRecording(cancel: cancel);
      if (_disposed) return;
      if (cancel || media == null) {
        stage = ComposerStage.idle;
        _notify();
        return;
      }
      if (_voiceDurationMs < 500) {
        _fail(ComposerFailureStage.voice, '录音太短，请按住说话');
        return;
      }
      _media = media;
      drafts.putAttachment(conv, AttachmentDraft.fromMedia(media));
      _uploadKey = newIdempotencyKey('voice-upload');
      _sendKey = newIdempotencyKey('voice-send');
      stage = ComposerStage.uploading;
      await _uploadAndSend(voice: true, durationMs: media.durationMs > 0 ? media.durationMs : _voiceDurationMs);
    } catch (error) {
      if (!_disposed) _fail(ComposerFailureStage.voice, '录音中断：${asG12Failure(error).displayText}');
    } finally {
      if (!recording) stage = ComposerStage.idle;
      _notify();
    }
  }

  Future<void> _uploadAndSend({bool voice = false, int durationMs = 0}) async {
    final media = _media;
    if (media == null) return;
    try {
      stage = ComposerStage.uploading;
      _notify();
      final uploaded = _uploaded ??= await attachments.upload(
        media,
        idempotencyKey: _uploadKey ??= newIdempotencyKey('media-upload'),
      );
      if (_disposed) return;
      drafts.putAttachment(conv, AttachmentDraft.fromMedia(media).withUploaded(uploaded.mediaId));
      stage = ComposerStage.sending;
      _notify();
      final outcome = voice
          ? await attachments.sendVoice(
              conv,
              uploaded,
              durationMs: durationMs,
              idempotencyKey: _sendKey ??= newIdempotencyKey('voice-send'),
            )
          : await attachments.sendMedia(
              conv,
              uploaded,
              idempotencyKey: _sendKey ??= newIdempotencyKey('media-send'),
            );
      if (_disposed) return;
      if (outcome.state == OutboxState.failed) {
        _fail(
          ComposerFailureStage.send,
          outcome.error.isEmpty ? '发送失败' : '发送失败：${outcome.error}',
        );
        return;
      }
      _media = null;
      _uploaded = null;
      _uploadKey = null;
      _sendKey = null;
      drafts.clearAttachment(conv);
      await onSent();
    } catch (error) {
      if (_disposed) return;
      // 上传阶段失败保留 _media，发送阶段失败保留 _uploaded，供重试。
      final stage = this.stage;
      if (stage == ComposerStage.uploading) {
        _fail(ComposerFailureStage.upload, '上传失败：${asG12Failure(error).displayText}');
      } else {
        _fail(ComposerFailureStage.send, '发送失败：${asG12Failure(error).displayText}');
      }
    } finally {
      stage = ComposerStage.idle;
      _notify();
    }
  }

  /// 上传/发送失败后的重试：只重做失败的那一段。
  Future<void> retryAttachment() async {
    if (busy) return;
    if (_media == null && _uploaded == null) return;
    failure = null;
    failureStage = null;
    _notify();
    await _uploadAndSend();
  }

  /// 网络恢复：只自动重试上传/发送失败，不重试权限/采集失败。
  Future<void> handleNetwork(bool online) async {
    if (!online) return;
    if (failureStage == ComposerFailureStage.upload ||
        failureStage == ComposerFailureStage.send) {
      await retryAttachment();
    }
  }

  Future<RedPacketComposerState?> openRedPacket() async {
    failure = null;
    failureStage = null;
    _notify();
    try {
      final state = await redPackets.current(conv);
      if (_disposed) return null;
      if (state == null) {
        _fail(ComposerFailureStage.redPacket, '没有可发送的红包：金额必须先在 G06 创建');
        return null;
      }
      redPacket = state;
      onOpenRedPacket?.call(state);
      return state;
    } catch (error) {
      if (!_disposed) _fail(ComposerFailureStage.redPacket, asG12Failure(error).displayText);
      return null;
    } finally {
      _notify();
    }
  }

  Future<void> openEntry(ComposerEntry entry) async {
    try {
      await entries.open(entry, conv);
    } catch (error) {
      _fail(ComposerFailureStage.unavailable, asG12Failure(error).displayText);
    }
  }

  Future<void> playVoice(String mediaId) async {
    try {
      await attachments.playVoice(mediaId);
    } catch (error) {
      _fail(ComposerFailureStage.unavailable, asG12Failure(error).displayText);
    }
  }

  Future<bool> _ensurePermission(PlatformPermission permission) async {
    final current = await platform.permissionStatus(permission);
    if (current.allowsUse) return true;
    final requested = await platform.requestPermission(permission);
    if (requested.allowsUse) return true;
    final permanent = requested == PermissionStatus.permanentlyDenied ||
        current == PermissionStatus.permanentlyDenied;
    _fail(
      ComposerFailureStage.permission,
      permanent
          ? '权限已被永久拒绝，请到系统设置开启'
          : '没有${_permissionName(permission)}权限，操作未开始',
    );
    return false;
  }

  Future<void> openPermissionSettings() => platform.openSettings(PlatformSettingsPage.app);

  String _permissionName(PlatformPermission permission) => switch (permission) {
        PlatformPermission.camera => '相机',
        PlatformPermission.microphone => '麦克风',
        PlatformPermission.photos => '相册',
        PlatformPermission.location => '定位',
        PlatformPermission.notifications => '通知',
        PlatformPermission.files => '文件',
      };

  String _captureMessage(Object error) {
    final failure = asG12Failure(error);
    if (failure.code == 'PERMISSION_DENIED') return failure.message;
    return '打开采集失败：${failure.displayText}';
  }

  void _fail(ComposerFailureStage stage, String message) {
    failureStage = stage;
    failure = message;
    _notify();
  }

  void _notify() {
    if (!_disposed) notifyListeners();
  }

  @override
  void dispose() {
    _disposed = true;
    if (recording) unawaited(platform.stopVoiceRecording(cancel: true));
    super.dispose();
  }
}
