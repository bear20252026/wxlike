import 'dart:async';

import 'package:flutter/foundation.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:wxlike_mobile/features/chat/composer/composer_controller.dart';
import 'package:wxlike_mobile/features/chat/composer/composer_ports.dart';
import 'package:wxlike_mobile/features/chat/composer/draft_store.dart';
import 'package:wxlike_mobile/platform/platform_ports.dart';
import 'package:wxlike_mobile/shell/contracts.dart';
import 'package:wxlike_mobile/src/service.dart';

import 'composer_support.dart';

ComposerController build({
  required DraftStore drafts,
  required FakePlatform platform,
  required FakeAttachments attachments,
  required FakeRedPackets redPackets,
  FakeEntries? entries,
  FakeActions? actions,
  Future<SendOutcome?> Function(String)? sendText,
  Future<void> Function()? onSent,
  ValueChanged<RedPacketComposerState>? onOpenRedPacket,
}) {
  return ComposerController(
    conv: 'c1',
    drafts: drafts,
    platform: platform,
    attachments: attachments,
    redPackets: redPackets,
    entries: entries ?? FakeEntries(),
    actions: actions ?? FakeActions(),
    sendTextHandler:
        sendText ??
        (text) async => const SendOutcome(
          taskId: 't',
          state: OutboxState.sent,
          msgId: 'm',
          error: '',
        ),
    onSent: onSent ?? () async {},
    onOpenRedPacket: onOpenRedPacket,
  );
}

CapturedMedia image() => const CapturedMedia(
  ref: 'file:///tmp/a.jpg',
  kind: MediaKind.image,
  bytes: 10,
);

class GatePersistence implements DraftPersistence {
  Map<String, Object?> data = const {};
  Completer<void>? gate;

  @override
  Future<Map<String, Object?>> load() async => const {};

  @override
  Future<void> save(Map<String, Object?> next) async {
    data = next;
    final pending = gate;
    if (pending != null) {
      gate = null;
      await pending.future;
    }
  }
}

void main() {
  late FakePlatform platform;
  late FakeAttachments attachments;
  late FakeRedPackets redPackets;
  late DraftStore drafts;
  late MemoryDraftPersistence persistence;

  setUp(() async {
    platform = FakePlatform();
    attachments = FakeAttachments();
    redPackets = FakeRedPackets();
    persistence = MemoryDraftPersistence();
    drafts = DraftStore(persistence: persistence, flushDelay: Duration.zero);
    await drafts.load();
  });

  test('权限被永久拒绝时不做采集，并指向系统设置', () async {
    platform.statuses[PlatformPermission.photos] = PermissionStatus.denied;
    platform.requests[PlatformPermission.photos] =
        PermissionStatus.permanentlyDenied;
    final controller = build(
      drafts: drafts,
      platform: platform,
      attachments: attachments,
      redPackets: redPackets,
    );

    await controller.pickAlbum();

    expect(platform.calls, isNot(contains('pickPhoto')));
    expect(controller.failureStage, ComposerFailureStage.permission);
    expect(controller.failure, contains('系统设置'));
    controller.dispose();
  });

  test('用户取消采集不是失败，也不发送', () async {
    platform.nextMedia = null;
    final controller = build(
      drafts: drafts,
      platform: platform,
      attachments: attachments,
      redPackets: redPackets,
    );

    await controller.pickAlbum();

    expect(controller.failure, isNull);
    expect(controller.failureStage, isNull);
    expect(attachments.uploadCalls, 0);
    expect(attachments.sendCalls, 0);
    controller.dispose();
  });

  test('上传失败保留媒体，重试只重做上传', () async {
    platform.nextMedia = image();
    attachments.uploadError = const G12Failure(
      G12FailureKind.offline,
      'NETWORK',
      '断网',
      retryable: true,
    );
    final controller = build(
      drafts: drafts,
      platform: platform,
      attachments: attachments,
      redPackets: redPackets,
    );

    await controller.pickAlbum();
    expect(controller.failureStage, ComposerFailureStage.upload);
    expect(drafts.attachment('c1'), isNotNull);

    attachments.uploadError = null;
    await controller.retryAttachment();

    expect(attachments.uploadCalls, 2);
    expect(attachments.sendCalls, 1);
    expect(controller.failure, isNull);
    expect(drafts.attachment('c1'), isNull);
    controller.dispose();
  });

  test('发送失败保留已上传媒体，重试不重复上传', () async {
    platform.nextMedia = image();
    attachments.sendError = const G12Failure(
      G12FailureKind.timeout,
      'TIMEOUT',
      '超时',
      retryable: true,
    );
    final controller = build(
      drafts: drafts,
      platform: platform,
      attachments: attachments,
      redPackets: redPackets,
    );

    await controller.pickAlbum();
    expect(controller.failureStage, ComposerFailureStage.send);
    expect(attachments.uploadCalls, 1);

    attachments.sendError = null;
    await controller.retryAttachment();

    expect(attachments.uploadCalls, 1);
    expect(attachments.sendCalls, 2);
    expect(drafts.attachment('c1'), isNull);
    controller.dispose();
  });

  test('进程重启后恢复未完成附件，并仍可重试', () async {
    platform.nextMedia = image();
    attachments.uploadError = const G12Failure(
      G12FailureKind.offline,
      'NETWORK',
      '断网',
      retryable: true,
    );
    final first = build(
      drafts: drafts,
      platform: platform,
      attachments: attachments,
      redPackets: redPackets,
    );
    await first.pickAlbum();
    await drafts.flush();
    first.dispose();

    final restoredPersistence = MemoryDraftPersistence()
      ..data = persistence.data;
    final restoredDrafts = DraftStore(persistence: restoredPersistence);
    await restoredDrafts.load();
    final restored = build(
      drafts: restoredDrafts,
      platform: platform,
      attachments: attachments,
      redPackets: redPackets,
    );

    expect(restored.failureStage, ComposerFailureStage.upload);
    expect(restoredDrafts.attachment('c1')?.ref, 'file:///tmp/a.jpg');
    attachments.uploadError = null;
    await restored.retryAttachment();
    expect(attachments.sendCalls, 1);
    restored.dispose();
  });

  test('语音滑动取消不产生上传，过短录音不假成功', () async {
    platform.nextMedia = const CapturedMedia(
      ref: 'voice',
      kind: MediaKind.audio,
      durationMs: 800,
    );
    final controller = build(
      drafts: drafts,
      platform: platform,
      attachments: attachments,
      redPackets: redPackets,
    );

    await controller.startVoiceRecording();
    controller.updateVoiceDrag(-120);
    expect(controller.cancelRecording, isTrue);
    await controller.finishVoiceRecording();
    expect(platform.calls, contains('voice.stop:cancel'));
    expect(attachments.uploadCalls, 0);

    await controller.startVoiceRecording();
    await controller.finishVoiceRecording();
    expect(controller.failureStage, ComposerFailureStage.voice);
    expect(controller.failure, contains('太短'));
    expect(attachments.uploadCalls, 0);
    controller.dispose();
  });

  test('表情入口转给 G13 owner action，不在输入器里复制业务', () async {
    final entries = FakeEntries();
    final controller = build(
      drafts: drafts,
      platform: platform,
      attachments: attachments,
      redPackets: redPackets,
      entries: entries,
    );

    await controller.openEntry(ComposerEntry.sticker);
    expect(entries.opened, [ComposerEntry.sticker]);
    controller.dispose();
  });

  test('红包金额只接受上游状态，客户端不随机', () async {
    final opened = <RedPacketComposerState>[];
    final controller = build(
      drafts: drafts,
      platform: platform,
      attachments: attachments,
      redPackets: redPackets,
      onOpenRedPacket: opened.add,
    );

    expect(await controller.openRedPacket(), isNull);
    expect(controller.failureStage, ComposerFailureStage.redPacket);

    redPackets.state = const RedPacketComposerState(
      packetId: 'p1',
      totalMinor: 1000,
      currency: 'CNY',
      greeting: '恭喜发财',
      status: 'CREATED',
    );
    final state = await controller.openRedPacket();
    expect(state?.packetId, 'p1');
    expect(opened.single.totalMinor, 1000);
    controller.dispose();
  });

  test('引用发送未接线时明确失败，不悄悄丢引用', () async {
    var sent = 0;
    final controller = build(
      drafts: drafts,
      platform: platform,
      attachments: attachments,
      redPackets: redPackets,
      sendText: (_) async {
        sent++;
        return const SendOutcome(
          taskId: 't',
          state: OutboxState.sent,
          msgId: 'm',
          error: '',
        );
      },
    );
    controller.updateText('你好');
    controller.quote(
      const ChatMessage(
        conv: 'c1',
        seq: 1,
        msgId: 'm1',
        sender: 'p',
        kind: 'text',
        text: '原消息',
        mediaId: '',
        sentAtMs: 1,
        withdrawn: false,
        mine: false,
      ),
    );
    await controller.sendText();

    expect(sent, 0);
    expect(controller.failureStage, ComposerFailureStage.unavailable);
    expect(controller.text, '你好');
    controller.dispose();
  });

  test('flush 期间的新输入不会被旧快照覆盖', () async {
    final gate = GatePersistence();
    final store = DraftStore(persistence: gate, flushDelay: const Duration(days: 1));
    await store.load();
    store.write('c1', '第一版');

    final blocker = Completer<void>();
    gate.gate = blocker;
    final first = store.flush();
    await Future<void>.delayed(Duration.zero);
    store.write('c1', '第二版');
    blocker.complete();
    await first;

    expect((gate.data['drafts'] as Map)['c1'], '第二版');
    store.dispose();
  });

  test('文字草稿跨进程恢复，发送成功后清空', () async {
    final controller = build(
      drafts: drafts,
      platform: platform,
      attachments: attachments,
      redPackets: redPackets,
    );
    controller.updateText('还没发出去的草稿');
    await drafts.flush();
    controller.dispose();

    final restored = DraftStore(persistence: persistence);
    await restored.load();
    expect(restored.read('c1'), '还没发出去的草稿');

    final second = build(
      drafts: restored,
      platform: platform,
      attachments: attachments,
      redPackets: redPackets,
    );
    expect(second.text, '还没发出去的草稿');
    await second.sendText();
    expect(restored.read('c1'), isEmpty);
    second.dispose();
  });
}
