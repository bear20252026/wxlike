import 'package:flutter/material.dart';

import '../../../shell/theme.dart';
import 'composer_controller.dart';
import 'composer_ports.dart';

class ComposerPanel extends StatefulWidget {
  const ComposerPanel({super.key, required this.controller});

  final ComposerController controller;

  @override
  State<ComposerPanel> createState() => _ComposerPanelState();
}

class _ComposerPanelState extends State<ComposerPanel> {
  late final TextEditingController _input =
      TextEditingController(text: widget.controller.text);

  @override
  void initState() {
    super.initState();
    widget.controller.addListener(_sync);
  }

  void _sync() {
    if (!mounted) return;
    if (_input.text != widget.controller.text) {
      _input.value = TextEditingValue(
        text: widget.controller.text,
        selection: TextSelection.collapsed(offset: widget.controller.text.length),
      );
    }
    setState(() {});
  }

  @override
  void dispose() {
    widget.controller.removeListener(_sync);
    _input.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final controller = widget.controller;
    return Material(
      color: Theme.of(context).brightness == Brightness.dark
          ? WxColors.darkSurface
          : const Color(0xffF7F7F7),
      child: SafeArea(
        top: false,
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            if (controller.failure != null) _failure(controller),
            if (controller.quoted != null) _quote(controller),
            if (controller.panelOpen) _panel(controller),
            if (controller.recording) _recordingHint(controller),
            Padding(
              padding: const EdgeInsets.fromLTRB(8, 6, 8, 8),
              child: Row(
                crossAxisAlignment: CrossAxisAlignment.end,
                children: [
                  IconButton(
                    key: const Key('composer-voice-toggle'),
                    onPressed: controller.busy
                        ? null
                        : () => controller.setVoiceMode(!controller.voiceMode),
                    icon: Icon(controller.voiceMode ? Icons.keyboard_alt_outlined : Icons.mic_none),
                    tooltip: controller.voiceMode ? '键盘' : '语音',
                  ),
                  Expanded(child: controller.voiceMode ? _voiceButton(controller) : _textField(controller)),
                  const SizedBox(width: 6),
                  if (!controller.voiceMode && controller.text.trim().isNotEmpty)
                    FilledButton(
                      key: const Key('composer-send'),
                      onPressed: controller.canSend ? controller.sendText : null,
                      style: FilledButton.styleFrom(
                        minimumSize: const Size(60, 38),
                        padding: const EdgeInsets.symmetric(horizontal: 14),
                      ),
                      child: controller.stage == ComposerStage.sending
                          ? const SizedBox(width: 14, height: 14, child: CircularProgressIndicator(strokeWidth: 2))
                          : const Text('发送', style: TextStyle(fontSize: 13)),
                    )
                  else
                    IconButton(
                      key: const Key('composer-attach-toggle'),
                      onPressed: controller.busy ? null : controller.togglePanel,
                      icon: Icon(
                        controller.panelOpen ? Icons.close : Icons.add_circle_outline,
                        color: controller.panelOpen ? WxColors.brand : null,
                      ),
                      tooltip: '更多',
                    ),
                ],
              ),
            ),
          ],
        ),
      ),
    );
  }

  Widget _textField(ComposerController controller) {
    return Container(
      constraints: const BoxConstraints(minHeight: 38, maxHeight: 120),
      decoration: BoxDecoration(
        color: Theme.of(context).colorScheme.surface,
        borderRadius: BorderRadius.circular(WxRadius.tile),
        border: Border.all(color: Theme.of(context).dividerColor.withValues(alpha: 0.4)),
      ),
      padding: const EdgeInsets.symmetric(horizontal: 10),
      child: TextField(
        key: const Key('composer-input'),
        controller: _input,
        minLines: 1,
        maxLines: 4,
        textInputAction: TextInputAction.newline,
        onChanged: controller.updateText,
        decoration: const InputDecoration(
          hintText: '说点什么…',
          border: InputBorder.none,
          enabledBorder: InputBorder.none,
          focusedBorder: InputBorder.none,
          filled: false,
          isDense: true,
          contentPadding: EdgeInsets.symmetric(vertical: 11),
        ),
      ),
    );
  }

  Widget _voiceButton(ComposerController controller) {
    return GestureDetector(
      key: const Key('composer-record'),
      onLongPressStart: controller.busy ? null : (_) => controller.startVoiceRecording(),
      onLongPressMoveUpdate:
          controller.busy ? null : (details) => controller.updateVoiceDrag(details.localOffsetFromOrigin.dy),
      onLongPressEnd: controller.busy ? null : (_) => controller.finishVoiceRecording(),
      onLongPressCancel: controller.busy ? null : controller.finishVoiceRecording,
      child: Container(
        height: 38,
        alignment: Alignment.center,
        decoration: BoxDecoration(
          color: controller.recording ? WxColors.brandSoft : Theme.of(context).colorScheme.surface,
          borderRadius: BorderRadius.circular(WxRadius.tile),
          border: Border.all(color: Theme.of(context).dividerColor.withValues(alpha: 0.4)),
        ),
        child: Text(
          controller.recording
              ? (controller.cancelRecording ? '松开取消' : '松开 发送')
              : (controller.stage == ComposerStage.capturing ? '录音中…' : '按住 说话'),
          style: TextStyle(
            fontSize: 13,
            color: controller.cancelRecording ? WxColors.danger : null,
          ),
        ),
      ),
    );
  }

  Widget _recordingHint(ComposerController controller) {
    return Container(
      width: double.infinity,
      color: controller.cancelRecording ? WxColors.danger.withValues(alpha: 0.12) : WxColors.brandSoft,
      padding: const EdgeInsets.symmetric(vertical: 6),
      child: Text(
        controller.cancelRecording ? '上滑取消录音' : '正在录音 · 上滑取消',
        textAlign: TextAlign.center,
        style: TextStyle(
          fontSize: 11,
          color: controller.cancelRecording ? WxColors.danger : WxColors.brandDark,
        ),
      ),
    );
  }

  Widget _quote(ComposerController controller) {
    final message = controller.quoted!;
    return Container(
      width: double.infinity,
      padding: const EdgeInsets.fromLTRB(12, 6, 4, 6),
      color: Theme.of(context).dividerColor.withValues(alpha: 0.15),
      child: Row(
        children: [
          Expanded(
            child: Text(
              '引用 ${message.sender}：${message.text}',
              maxLines: 1,
              overflow: TextOverflow.ellipsis,
              style: const TextStyle(fontSize: 11),
            ),
          ),
          IconButton(
            onPressed: controller.clearQuote,
            icon: const Icon(Icons.close, size: 15),
            visualDensity: VisualDensity.compact,
          ),
        ],
      ),
    );
  }

  Widget _failure(ComposerController controller) {
    final retryable = controller.failureStage == ComposerFailureStage.upload ||
        controller.failureStage == ComposerFailureStage.send;
    return Container(
      width: double.infinity,
      margin: const EdgeInsets.fromLTRB(10, 6, 10, 0),
      padding: const EdgeInsets.fromLTRB(10, 6, 4, 6),
      decoration: BoxDecoration(
        color: WxColors.danger.withValues(alpha: 0.1),
        borderRadius: BorderRadius.circular(6),
      ),
      child: Row(
        children: [
          Expanded(
            child: Text(
              controller.failure!,
              style: const TextStyle(fontSize: 11, color: WxColors.danger),
            ),
          ),
          if (retryable)
            TextButton(
              key: const Key('composer-retry'),
              onPressed: controller.retryAttachment,
              child: const Text('重试'),
            ),
        ],
      ),
    );
  }

  Widget _panel(ComposerController controller) {
    final items = <_Entry>[
      _Entry('相册', Icons.photo_outlined, controller.pickAlbum, key: 'attach-album'),
      _Entry('拍摄', Icons.photo_camera_outlined, controller.takePhoto, key: 'attach-camera'),
      _Entry('文件', Icons.folder_outlined, controller.pickFile, key: 'attach-file'),
      _Entry('位置', Icons.place_outlined, () => controller.openEntry(ComposerEntry.location), key: 'attach-location'),
      _Entry('红包', Icons.card_giftcard_outlined, controller.openRedPacket, key: 'attach-redpacket'),
      _Entry('通话', Icons.videocam_outlined, () => controller.openEntry(ComposerEntry.call), key: 'attach-call'),
      _Entry('转账', Icons.swap_horiz, () => controller.openEntry(ComposerEntry.transfer), key: 'attach-transfer'),
      _Entry('收藏', Icons.star_border, () => controller.openEntry(ComposerEntry.favorite), key: 'attach-favorite'),
    ];
    return Container(
      width: double.infinity,
      padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 14),
      decoration: BoxDecoration(
        color: Theme.of(context).colorScheme.surface,
        border: Border(top: BorderSide(color: Theme.of(context).dividerColor)),
      ),
      child: Wrap(
        spacing: 18,
        runSpacing: 14,
        children: [
          for (final item in items)
            SizedBox(
              width: 58,
              child: InkWell(
                key: Key(item.key),
                onTap: controller.busy ? null : item.onTap,
                borderRadius: BorderRadius.circular(10),
                child: Column(
                  children: [
                    Container(
                      width: 50,
                      height: 50,
                      decoration: BoxDecoration(
                        color: Theme.of(context).dividerColor.withValues(alpha: 0.15),
                        borderRadius: BorderRadius.circular(14),
                      ),
                      child: Icon(item.icon, size: 22),
                    ),
                    const SizedBox(height: 5),
                    Text(item.label, style: const TextStyle(fontSize: 10)),
                  ],
                ),
              ),
            ),
        ],
      ),
    );
  }
}

class _Entry {
  _Entry(this.label, this.icon, this.onTap, {required this.key});

  final String label;
  final IconData icon;
  final Future<void> Function() onTap;
  final String key;
}
