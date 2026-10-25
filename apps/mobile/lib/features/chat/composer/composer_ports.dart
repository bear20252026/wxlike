/// 输入器的外部能力：上传/发送媒体、红包状态、消息菜单动作。
///
/// 上传属于 G04、红包属于 G06、转发撤回属于 G03；这些上游未接线时端口必须
/// 明确抛错，输入器显示对应阶段失败，不能把"没上传"伪装成"已发送"。
library;

import '../../../platform/platform_ports.dart';
import '../../../shell/contracts.dart';
import '../../../src/service.dart';

class UploadedMedia {
  const UploadedMedia({
    required this.mediaId,
    required this.kind,
    this.width = 0,
    this.height = 0,
    this.durationMs = 0,
  });

  final String mediaId;
  final MediaKind kind;
  final int width;
  final int height;
  final int durationMs;
}

abstract class AttachmentPort {
  Future<UploadedMedia> upload(
    CapturedMedia media, {
    required String idempotencyKey,
  });

  Future<SendOutcome> sendMedia(
    String conv,
    UploadedMedia media, {
    required String idempotencyKey,
  });

  Future<SendOutcome> sendVoice(
    String conv,
    UploadedMedia media, {
    required int durationMs,
    required String idempotencyKey,
  });

  Future<void> playVoice(String mediaId);
}

class UnavailableAttachmentPort implements AttachmentPort {
  const UnavailableAttachmentPort();

  Never _notWired(String method) =>
      throw G12Failure.upstreamNotWired(method, '$method 尚未接入 G04 绑定');

  @override
  Future<UploadedMedia> upload(CapturedMedia media, {required String idempotencyKey}) =>
      _notWired('media.upload');

  @override
  Future<SendOutcome> sendMedia(
    String conv,
    UploadedMedia media, {
    required String idempotencyKey,
  }) =>
      _notWired('messaging.send_media');

  @override
  Future<SendOutcome> sendVoice(
    String conv,
    UploadedMedia media, {
    required int durationMs,
    required String idempotencyKey,
  }) =>
      _notWired('messaging.send_voice');

  @override
  Future<void> playVoice(String mediaId) => _notWired('media.play');
}

class RedPacketComposerState {
  const RedPacketComposerState({
    required this.packetId,
    required this.totalMinor,
    required this.currency,
    required this.greeting,
    required this.status,
  });

  final String packetId;
  final int totalMinor;
  final String currency;
  final String greeting;

  /// `CREATED` / `SENT` / `CLAIMED` / `EXPIRED`；输入器只认服务端状态。
  final String status;
}

abstract class RedPacketComposerPort {
  /// 返回本会话可发送的红包；没有则返回 null。金额只能来自 G06，客户端不得随机。
  Future<RedPacketComposerState?> current(String conv);
}

class UnavailableRedPacketComposerPort implements RedPacketComposerPort {
  const UnavailableRedPacketComposerPort();

  @override
  Future<RedPacketComposerState?> current(String conv) =>
      throw G12Failure.upstreamNotWired('commerce.red_packet', '红包状态尚未接入 G06 绑定');
}

abstract class MessageActionsPort {
  Future<void> forward(
    String messageId,
    String targetConv, {
    required String idempotencyKey,
  });

  Future<void> withdraw(String messageId, {required String idempotencyKey});
}

class UnavailableMessageActionsPort implements MessageActionsPort {
  const UnavailableMessageActionsPort();

  @override
  Future<void> forward(
    String messageId,
    String targetConv, {
    required String idempotencyKey,
  }) =>
      throw G12Failure.upstreamNotWired('messaging.forward');

  @override
  Future<void> withdraw(String messageId, {required String idempotencyKey}) =>
      throw G12Failure.upstreamNotWired('messaging.withdraw');
}

/// 位置/通话/收藏等入口由各自 owner 提供；输入器只转交，不自己画假页面。
enum ComposerEntry { sticker, location, call, favorite, transfer, redPacket }

abstract class ComposerEntryPort {
  Future<void> open(ComposerEntry entry, String conv);
}

class UnavailableComposerEntryPort implements ComposerEntryPort {
  const UnavailableComposerEntryPort();

  @override
  Future<void> open(ComposerEntry entry, String conv) => throw G12Failure.upstreamNotWired(
        'entry.${entry.name}',
        '${entry.name} 入口由对应职责组提供，尚未接线',
      );
}
