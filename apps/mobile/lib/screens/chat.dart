import 'dart:async';

import 'package:flutter/material.dart';

import '../features/chat/timeline/messaging_ports.dart';
import '../src/service.dart';
import '../src/widgets.dart';

/// 聊天屏。
///
/// 气泡的文案（"你撤回了一条消息"）、排序、离线兜底、待发队列都是 Rust 的结果；
/// 这里只做两件事：把 `ChatMessage` 画出来，把用户输入交给 `sendText`。
class ChatScreen extends StatefulWidget {
  const ChatScreen({super.key, required this.service, required this.row});

  final WxlikeService service;
  final ConversationRow row;

  @override
  State<ChatScreen> createState() => _ChatScreenState();
}

class _ChatScreenState extends State<ChatScreen> {
  final _input = TextEditingController();
  final _scroll = ScrollController();

  List<ChatMessage> _messages = const [];
  List<OutboxEntry> _pending = const [];
  int _cursor = -1;
  bool _fromCache = false;
  bool _busy = true;
  bool _sending = false;
  String? _error;
  StreamSubscription<ChatEvent>? _watchSub;

  @override
  void initState() {
    super.initState();
    _open();
    // 接到 FfiMessagingPort 的 watch 上：新屏（G12 时间线）靠它补齐了
    // "聊天屏没有实时接收"，旧屏同样要能收到新消息，否则退回旧壳就是退步。
    _watchSub = FfiMessagingPort(widget.service)
        .watch(widget.row.conv)
        .listen(_onWatchEvent);
  }

  @override
  void dispose() {
    _watchSub?.cancel();
    _input.dispose();
    _scroll.dispose();
    super.dispose();
  }

  void _onWatchEvent(ChatEvent event) {
    if (!mounted) return;
    switch (event) {
      case MessagesUpdated(:final messages):
        setState(() => _messages = _mergeById(_messages, messages));
        if (_nearBottom()) _jumpToBottom();
      case OutboxUpdated(:final summary):
        setState(() {
          _pending = summary.entries.where((e) => e.conv == widget.row.conv).toList();
        });
      case UnreadUpdated():
      case ConversationsUpdated():
      case ChatWatchFailed():
        // 轮询失败由 _load 的错误条负责呈现，这里不重复打断用户。
        break;
    }
  }

  bool _nearBottom() {
    if (!_scroll.hasClients) return true;
    return _scroll.position.maxScrollExtent - _scroll.position.pixels < 140;
  }

  /// 按 msgId 合并并按 seq 排序：反向翻页与轮询推来的最新一页都可能
  /// 与已有列表重叠，直接拼接会出现重复气泡。
  List<ChatMessage> _mergeById(List<ChatMessage> base, List<ChatMessage> incoming) {
    final byId = <String, ChatMessage>{
      for (final message in base) message.msgId: message,
    };
    for (final message in incoming) {
      byId[message.msgId] = message;
    }
    final merged = byId.values.toList()
      ..sort((a, b) {
        final bySeq = a.seq.compareTo(b.seq);
        return bySeq != 0 ? bySeq : a.msgId.compareTo(b.msgId);
      });
    return List<ChatMessage>.unmodifiable(merged);
  }

  Future<void> _open() async {
    await _load();
    if (!mounted) return;
    try {
      // 已读游标推到最新；失败只提示，不影响已经画出来的历史
      await widget.service.markRead(widget.row.conv, 0);
    } catch (error) {
      if (mounted) setState(() => _error = describeError(error));
    }
  }

  Future<void> _load({bool older = false}) async {
    setState(() {
      _busy = true;
      _error = null;
    });
    try {
      final before = older ? _cursor : 0;
      final page = await widget.service.history(widget.row.conv, before, pageSize);
      final summary = await widget.service.outboxStatus();
      if (!mounted) return;
      setState(() {
        _cursor = page.nextBeforeSeq;
        _fromCache = page.fromCache;
        // 更早的一页可能与已加载的历史重叠：按 msgId 去重后再合并。
        _messages = older ? _mergeById(page.messages, _messages) : page.messages;
        _pending = summary.entries.where((e) => e.conv == widget.row.conv).toList();
        _busy = false;
      });
      if (!older) _jumpToBottom();
    } catch (error) {
      if (!mounted) return;
      setState(() {
        _error = describeError(error);
        _busy = false;
      });
    }
  }

  void _jumpToBottom() {
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (_scroll.hasClients) _scroll.jumpTo(_scroll.position.maxScrollExtent);
    });
  }

  Future<void> _send() async {
    final text = _input.text.trim();
    if (text.isEmpty || _sending) return;
    setState(() => _sending = true);
    try {
      final outcome = await widget.service.sendText(widget.row.conv, text);
      if (!mounted) return;
      _input.clear();
      if (outcome.state == OutboxState.sent) {
        await _load();
      } else {
        // Pending / Failed 都还在队列里：不重拉历史，只刷队列气泡
        final summary = await widget.service.outboxStatus();
        if (!mounted) return;
        setState(() => _pending = summary.entries.where((e) => e.conv == widget.row.conv).toList());
      }
    } catch (error) {
      if (mounted) setState(() => _error = describeError(error));
    } finally {
      if (mounted) setState(() => _sending = false);
    }
  }

  Future<void> _retry(String taskId) async {
    final summary = await widget.service.retryOutbox(taskId);
    if (!mounted) return;
    setState(() => _pending = summary.entries.where((e) => e.conv == widget.row.conv).toList());
  }

  Future<void> _drop(String taskId) async {
    await widget.service.dropOutbox(taskId);
    final summary = await widget.service.outboxStatus();
    if (!mounted) return;
    setState(() => _pending = summary.entries.where((e) => e.conv == widget.row.conv).toList());
  }

  @override
  Widget build(BuildContext context) {
    final bubbles = <Widget>[
      if (_cursor > 0)
        TextButton(
          onPressed: _busy ? null : () => _load(older: true),
          child: const Text('看更早的消息'),
        ),
      for (final message in _messages) _bubble(message),
      for (final entry in _pending) _pendingBubble(entry),
    ];
    return Scaffold(
      appBar: AppBar(
        title: Text(widget.row.title),
        bottom: PreferredSize(
          preferredSize: Size.fromHeight(_fromCache ? 28 : 0),
          child: _fromCache
              ? const ColoredBox(
                  color: Color(0x22ff9800),
                  child: Padding(
                    padding: EdgeInsets.symmetric(vertical: 4, horizontal: 16),
                    child: Text('离线中 · 显示的是本地缓存', textAlign: TextAlign.center),
                  ),
                )
              : const SizedBox.shrink(),
        ),
      ),
      body: Column(
        children: [
          if (_error != null)
            Padding(padding: const EdgeInsets.all(12), child: ErrorBox(message: _error!)),
          Expanded(
            child: _busy && _messages.isEmpty && _pending.isEmpty
                ? const Center(child: CircularProgressIndicator())
                : ListView(
                    controller: _scroll,
                    padding: const EdgeInsets.all(12),
                    children: bubbles,
                  ),
          ),
          _composer(),
        ],
      ),
    );
  }

  Widget _composer() {
    return SafeArea(
      child: Padding(
        padding: const EdgeInsets.fromLTRB(12, 4, 12, 12),
        child: Row(
          children: [
            Expanded(
              child: TextField(
                controller: _input,
                minLines: 1,
                maxLines: 4,
                textInputAction: TextInputAction.send,
                onSubmitted: (_) => _send(),
                decoration: const InputDecoration(border: OutlineInputBorder()),
              ),
            ),
            const SizedBox(width: 8),
            FilledButton(
              onPressed: _sending ? null : _send,
              child: _sending
                  ? const SizedBox(height: 16, child: CircularProgressIndicator(strokeWidth: 2))
                  : const Text('发送'),
            ),
          ],
        ),
      ),
    );
  }

  Widget _bubble(ChatMessage message) {
    final scheme = Theme.of(context).colorScheme;
    final color = message.mine ? scheme.primaryContainer : scheme.surfaceContainerHighest;
    return Align(
      alignment: message.mine ? Alignment.centerRight : Alignment.centerLeft,
      child: Container(
        margin: const EdgeInsets.symmetric(vertical: 4),
        padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 8),
        constraints: const BoxConstraints(maxWidth: 320),
        decoration: BoxDecoration(color: color, borderRadius: BorderRadius.circular(10)),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            if (!message.mine)
              Text(message.sender, style: const TextStyle(fontSize: 11, color: Colors.black54)),
            Text(message.text.isEmpty ? '[${message.kind}]' : message.text),
          ],
        ),
      ),
    );
  }

  Widget _pendingBubble(OutboxEntry entry) {
    final failed = entry.state == OutboxState.failed;
    final label = failed ? '发送失败' : '待发送';
    return Align(
      alignment: Alignment.centerRight,
      child: Container(
        margin: const EdgeInsets.symmetric(vertical: 4),
        padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 8),
        constraints: const BoxConstraints(maxWidth: 320),
        decoration: BoxDecoration(
          color: failed ? const Color(0x33ff5252) : const Color(0x22ffc107),
          borderRadius: BorderRadius.circular(10),
        ),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.end,
          children: [
            Text(entry.text),
            const SizedBox(height: 2),
            Row(
              mainAxisSize: MainAxisSize.min,
              children: [
                Text('$label · 第 ${entry.attempts + 1} 次', style: const TextStyle(fontSize: 11)),
                TextButton(
                  onPressed: () => _retry(entry.taskId),
                  child: const Text('重试'),
                ),
                TextButton(onPressed: () => _drop(entry.taskId), child: const Text('删除')),
              ],
            ),
            if (failed && entry.lastError.isNotEmpty)
              SizedBox(
                width: 260,
                child: Text(entry.lastError, style: const TextStyle(fontSize: 10)),
              ),
          ],
        ),
      ),
    );
  }
}
