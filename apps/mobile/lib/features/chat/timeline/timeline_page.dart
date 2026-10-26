import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../../../platform/platform_ports.dart';
import '../../../shell/contracts.dart';
import '../../../shell/theme.dart';
import '../../../shell/widgets.dart';
import '../../../src/service.dart';
import '../composer/composer_controller.dart';
import '../composer/composer_panel.dart';
import '../composer/composer_ports.dart';
import '../composer/draft_store.dart';
import 'timeline_controller.dart';

class TimelinePage extends StatefulWidget {
  const TimelinePage({
    super.key,
    required this.controller,
    required this.drafts,
    required this.platform,
    required this.attachments,
    required this.redPackets,
    required this.entries,
    required this.actions,
    required this.lifecycle,
    required this.networkStatus,
    this.title = '会话',
    this.isGroup = false,
  });

  final String title;
  final TimelineController controller;
  final DraftStore drafts;
  final PlatformPort platform;
  final AttachmentPort attachments;
  final RedPacketComposerPort redPackets;
  final ComposerEntryPort entries;
  final MessageActionsPort actions;
  final LifecyclePort lifecycle;
  final Stream<NetworkStatus> networkStatus;
  final bool isGroup;

  @override
  State<TimelinePage> createState() => _TimelinePageState();
}

class _TimelinePageState extends State<TimelinePage> {
  late final ComposerController _composer = ComposerController(
    conv: widget.controller.conv,
    drafts: widget.drafts,
    platform: widget.platform,
    attachments: widget.attachments,
    redPackets: widget.redPackets,
    entries: widget.entries,
    actions: widget.actions,
    sendTextHandler: widget.controller.send,
    onSent: widget.controller.refresh,
  );
  final _scroll = ScrollController();
  final Set<String> _selected = {};
  StreamSubscription<AppLifecycleSignal>? _lifecycleSub;
  StreamSubscription<NetworkStatus>? _networkSub;
  bool _atBottom = true;

  @override
  void initState() {
    super.initState();
    widget.controller.addListener(_onTimeline);
    _composer.addListener(_onComposer);
    _scroll.addListener(_onScroll);
    widget.controller.init().then((_) {
      if (mounted) _jumpToBottom();
    });
    _lifecycleSub = widget.lifecycle.signals.listen((signal) {
      if (!mounted) return;
      final foreground = signal == AppLifecycleSignal.resumed;
      widget.controller.setForeground(foreground);
      if (foreground) unawaited(widget.controller.onSignal(ChatSignal.resumed));
    });
    _networkSub = widget.networkStatus.listen(
      (status) {
        if (!mounted) return;
        if (status == NetworkStatus.online) {
          unawaited(widget.controller.onSignal(ChatSignal.networkRegained));
          unawaited(_composer.handleNetwork(true));
        }
      },
      onError: (Object error) {
        // 网络事件流异常（平台桥缺失/中断）不应拖垮页面：端口层对
        // PLATFORM_UNWIRED 已按一次"离线"兜底，这里无需再动作。
      },
    );
  }

  void _onTimeline() {
    if (!mounted) return;
    if (_atBottom) _jumpToBottom();
    setState(() {});
  }

  void _onComposer() {
    if (mounted) setState(() {});
  }

  void _onScroll() {
    if (!_scroll.hasClients) return;
    final position = _scroll.position;
    _atBottom = position.maxScrollExtent - position.pixels < 140;
    if (position.pixels < 120 && widget.controller.canLoadOlder) {
      unawaited(widget.controller.loadOlder());
    }
  }

  void _jumpToBottom() {
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (_scroll.hasClients) _scroll.jumpTo(_scroll.position.maxScrollExtent);
    });
  }

  @override
  void dispose() {
    widget.controller.removeListener(_onTimeline);
    _composer.removeListener(_onComposer);
    _lifecycleSub?.cancel();
    _networkSub?.cancel();
    _composer.dispose();
    _scroll.dispose();
    widget.controller.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final controller = widget.controller;
    return Scaffold(
      appBar: _selected.isEmpty ? _appBar(controller) : _selectionBar(),
      body: Column(
        children: [
          if (controller.fromCache)
            Container(
              width: double.infinity,
              color: WxColors.warning.withValues(alpha: 0.15),
              padding: const EdgeInsets.symmetric(vertical: 4),
              child: const Text('离线中 · 显示本地缓存', textAlign: TextAlign.center, style: TextStyle(fontSize: 11)),
            ),
          if (controller.error != null)
            Padding(
              padding: const EdgeInsets.fromLTRB(10, 8, 10, 0),
              child: G12ErrorBanner(
                message: controller.error!.displayText,
                onRetry: controller.error!.retryable || controller.error!.kind == G12FailureKind.offline
                    ? controller.refresh
                    : null,
              ),
            ),
          Expanded(child: _messageList(controller)),
          ComposerPanel(controller: _composer),
        ],
      ),
    );
  }

  PreferredSizeWidget _appBar(TimelineController controller) {
    return AppBar(
      leading: BackButton(onPressed: () => Navigator.of(context).maybePop()),
      title: Column(
        children: [
          Text(widget.title, style: const TextStyle(fontSize: 15)),
          if (widget.isGroup)
            const Text('群聊', style: TextStyle(fontSize: 9, color: WxColors.textSecondary)),
        ],
      ),
      actions: [
        IconButton(
          onPressed: controller.refresh,
          icon: const Icon(Icons.refresh, size: 20),
          tooltip: '刷新',
        ),
      ],
    );
  }

  PreferredSizeWidget _selectionBar() {
    return AppBar(
      leading: IconButton(
        onPressed: () => setState(_selected.clear),
        icon: const Icon(Icons.close),
      ),
      title: Text('已选择 ${_selected.length} 条'),
      actions: [
        TextButton(
          onPressed: _selected.isEmpty ? null : _forwardSelected,
          child: const Text('转发'),
        ),
      ],
    );
  }

  Widget _messageList(TimelineController controller) {
    if (controller.busy && controller.messages.isEmpty && controller.pending.isEmpty) {
      return const Center(child: CircularProgressIndicator());
    }
    if (!controller.busy && controller.messages.isEmpty && controller.pending.isEmpty) {
      return G12EmptyState(
        title: controller.error == null ? '还没有消息' : '消息未能加载',
        hint: controller.error?.displayText,
        action: controller.error == null
            ? null
            : OutlinedButton(onPressed: controller.init, child: const Text('重试')),
      );
    }
    final items = <Widget>[];
    if ((controller.nextBeforeSeq ?? -1) > 0) {
      items.add(
        Center(
          child: TextButton(
            onPressed: controller.canLoadOlder ? controller.loadOlder : null,
            child: controller.loadingOlder
                ? const SizedBox(width: 16, height: 16, child: CircularProgressIndicator(strokeWidth: 2))
                : Text(controller.canLoadOlder ? '查看更早消息' : '已达到本地缓存上限'),
          ),
        ),
      );
    }
    for (final message in controller.messages) {
      if (controller.firstUnreadSeq != null && message.seq == controller.firstUnreadSeq) {
        items.add(const _UnreadDivider());
      }
      items.add(_bubble(message));
    }
    for (final entry in controller.pending) {
      items.add(_pendingBubble(entry));
    }
    return ListView(
      controller: _scroll,
      padding: const EdgeInsets.fromLTRB(12, 10, 12, 10),
      children: items,
    );
  }

  Widget _bubble(ChatMessage message) {
    final mine = message.mine;
    final selected = _selected.contains(message.msgId);
    final dark = Theme.of(context).brightness == Brightness.dark;
    final color = mine
        ? (dark ? WxColors.darkChatMine : WxColors.chatMine)
        : (dark ? WxColors.darkSurface : Colors.white);
    final content = message.withdrawn
        ? '消息已撤回'
        : (message.text.isNotEmpty ? message.text : _kindLabel(message.kind));
    return Align(
      alignment: mine ? Alignment.centerRight : Alignment.centerLeft,
      child: GestureDetector(
        key: Key('msg-${message.msgId}'),
        onLongPress: _selected.isEmpty ? () => _showMenu(message) : null,
        onTap: _selected.isEmpty
            ? (message.kind == 'voice' || message.kind == 'audio'
                ? () => _composer.playVoice(message.mediaId)
                : null)
            : () => setState(() {
                  if (!_selected.remove(message.msgId)) _selected.add(message.msgId);
                }),
        child: Container(
          margin: const EdgeInsets.symmetric(vertical: 5),
          padding: const EdgeInsets.symmetric(horizontal: 11, vertical: 8),
          constraints: BoxConstraints(maxWidth: MediaQuery.sizeOf(context).width * 0.72),
          decoration: BoxDecoration(
            color: color,
            borderRadius: BorderRadius.circular(WxRadius.bubble),
            border: selected ? Border.all(color: WxColors.brand, width: 1.5) : null,
          ),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              if (widget.isGroup && !mine)
                Padding(
                  padding: const EdgeInsets.only(bottom: 2),
                  child: Text(
                    message.sender,
                    style: TextStyle(
                      fontSize: 10,
                      color: dark ? WxColors.darkTextSecondary : WxColors.textSecondary,
                    ),
                  ),
                ),
              Row(
                mainAxisSize: MainAxisSize.min,
                children: [
                  if (_selected.contains(message.msgId))
                    const Padding(
                      padding: EdgeInsets.only(right: 6),
                      child: Icon(Icons.check_circle, size: 14, color: WxColors.brand),
                    ),
                  Flexible(
                    child: Text(
                      content,
                      style: TextStyle(
                        fontSize: 14,
                        fontStyle: message.withdrawn ? FontStyle.italic : FontStyle.normal,
                      ),
                    ),
                  ),
                ],
              ),
              const SizedBox(height: 2),
              Text(
                formatMessageTime(message.sentAtMs),
                style: TextStyle(
                  fontSize: 8,
                  color: dark ? WxColors.darkTextSecondary : WxColors.textTertiary,
                ),
              ),
            ],
          ),
        ),
      ),
    );
  }

  Widget _pendingBubble(OutboxEntry entry) {
    final failed = entry.state == OutboxState.failed;
    final label = switch (entry.state) {
      OutboxState.sent => '已发送',
      OutboxState.failed => '发送失败',
      OutboxState.pending => '待发送',
    };
    return Align(
      alignment: Alignment.centerRight,
      child: Container(
        margin: const EdgeInsets.symmetric(vertical: 5),
        padding: const EdgeInsets.fromLTRB(11, 8, 6, 6),
        constraints: BoxConstraints(maxWidth: MediaQuery.sizeOf(context).width * 0.78),
        decoration: BoxDecoration(
          color: failed ? WxColors.danger.withValues(alpha: 0.12) : WxColors.warning.withValues(alpha: 0.16),
          borderRadius: BorderRadius.circular(WxRadius.bubble),
        ),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.end,
          children: [
            Text(
              entry.text.isEmpty ? _kindLabel(entry.kind) : entry.text,
              style: const TextStyle(fontSize: 14),
            ),
            const SizedBox(height: 2),
            Row(
              mainAxisSize: MainAxisSize.min,
              children: [
                Text('$label · 第 ${entry.attempts + 1} 次', style: const TextStyle(fontSize: 9)),
                TextButton(
                  onPressed: widget.controller.retrying.contains(entry.taskId)
                      ? null
                      : () => widget.controller.retry(entry.taskId),
                  style: TextButton.styleFrom(visualDensity: VisualDensity.compact),
                  child: const Text('重试', style: TextStyle(fontSize: 10)),
                ),
                TextButton(
                  onPressed: () => widget.controller.drop(entry.taskId),
                  style: TextButton.styleFrom(visualDensity: VisualDensity.compact),
                  child: const Text('删除', style: TextStyle(fontSize: 10)),
                ),
              ],
            ),
            if (failed && entry.lastError.isNotEmpty)
              Text(entry.lastError, style: const TextStyle(fontSize: 9)),
          ],
        ),
      ),
    );
  }

  String _kindLabel(String kind) => switch (kind) {
        'image' => '[图片]',
        'voice' || 'audio' => '[语音]',
        'video' => '[视频]',
        'file' => '[文件]',
        'location' => '[位置]',
        'redpacket' => '[红包]',
        'transfer' => '[转账]',
        _ => '[$kind]',
      };

  Future<void> _showMenu(ChatMessage message) async {
    final action = await showModalBottomSheet<String>(
      context: context,
      showDragHandle: true,
      builder: (context) => SafeArea(
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            ListTile(
              leading: const Icon(Icons.copy),
              title: const Text('复制'),
              onTap: () => Navigator.pop(context, 'copy'),
            ),
            ListTile(
              leading: const Icon(Icons.format_quote),
              title: const Text('引用'),
              onTap: () => Navigator.pop(context, 'quote'),
            ),
            ListTile(
              leading: const Icon(Icons.forward),
              title: const Text('转发'),
              onTap: () => Navigator.pop(context, 'forward'),
            ),
            if (message.mine)
              ListTile(
                leading: const Icon(Icons.undo),
                title: const Text('撤回'),
                onTap: () => Navigator.pop(context, 'withdraw'),
              ),
            ListTile(
              leading: const Icon(Icons.checklist),
              title: const Text('多选'),
              onTap: () => Navigator.pop(context, 'select'),
            ),
          ],
        ),
      ),
    );
    if (!mounted || action == null) return;
    switch (action) {
      case 'copy':
        await Clipboard.setData(ClipboardData(text: message.text));
        if (mounted) ScaffoldMessenger.of(context).showSnackBar(const SnackBar(content: Text('已复制')));
      case 'quote':
        _composer.quote(message);
      case 'forward':
        await _forward([message.msgId]);
      case 'withdraw':
        await _withdraw(message);
      case 'select':
        setState(() => _selected.add(message.msgId));
    }
  }

  Future<void> _withdraw(ChatMessage message) async {
    final confirmed = await showDialog<bool>(
      context: context,
      builder: (context) => AlertDialog(
        title: const Text('撤回这条消息？'),
        content: const Text('对方会看到"消息已撤回"。'),
        actions: [
          TextButton(onPressed: () => Navigator.pop(context, false), child: const Text('取消')),
          FilledButton(onPressed: () => Navigator.pop(context, true), child: const Text('撤回')),
        ],
      ),
    );
    if (confirmed != true || !mounted) return;
    try {
      await widget.actions.withdraw(message.msgId, idempotencyKey: newIdempotencyKey('withdraw'));
      await widget.controller.refresh();
    } catch (error) {
      if (mounted) {
        ScaffoldMessenger.of(context).showSnackBar(SnackBar(content: Text(asG12Failure(error).displayText)));
      }
    }
  }

  Future<void> _forwardSelected() async {
    final ids = _selected.toList(growable: false);
    if (ids.isEmpty) return;
    await _forward(ids);
    if (mounted) setState(_selected.clear);
  }

  Future<void> _forward(List<String> messageIds) async {
    final target = await showDialog<String>(
      context: context,
      builder: (context) => const _ForwardDialog(),
    );
    if (target == null || target.trim().isEmpty || !mounted) return;
    try {
      for (final id in messageIds) {
        await widget.actions.forward(
          id,
          target.trim(),
          idempotencyKey: newIdempotencyKey('forward'),
        );
      }
      if (mounted) {
        ScaffoldMessenger.of(context).showSnackBar(SnackBar(content: Text('已转发 ${messageIds.length} 条')));
      }
    } catch (error) {
      if (mounted) {
        ScaffoldMessenger.of(context).showSnackBar(SnackBar(content: Text(asG12Failure(error).displayText)));
      }
    }
  }
}

class _UnreadDivider extends StatelessWidget {
  const _UnreadDivider();

  @override
  Widget build(BuildContext context) {
    return Padding(
      padding: const EdgeInsets.symmetric(vertical: 8),
      child: Row(
        children: [
          const Expanded(child: Divider()),
          Container(
            margin: const EdgeInsets.symmetric(horizontal: 8),
            padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 2),
            decoration: BoxDecoration(
              color: WxColors.brandSoft,
              borderRadius: BorderRadius.circular(10),
            ),
            child: const Text('以下为新消息', style: TextStyle(fontSize: 9, color: WxColors.brandDark)),
          ),
          const Expanded(child: Divider()),
        ],
      ),
    );
  }
}

class _ForwardDialog extends StatefulWidget {
  const _ForwardDialog();

  @override
  State<_ForwardDialog> createState() => _ForwardDialogState();
}

class _ForwardDialogState extends State<_ForwardDialog> {
  late final TextEditingController _target = TextEditingController();

  @override
  void dispose() {
    _target.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    return AlertDialog(
      title: const Text('转发到会话'),
      content: TextField(
        key: const Key('forward-target'),
        controller: _target,
        decoration: const InputDecoration(
          labelText: '会话 id',
          helperText: '需要目标会话的 conv id；会话搜索由 G02/G03 提供',
        ),
      ),
      actions: [
        TextButton(onPressed: () => Navigator.pop(context), child: const Text('取消')),
        FilledButton(
          onPressed: () => Navigator.pop(context, _target.text),
          child: const Text('转发'),
        ),
      ],
    );
  }
}
