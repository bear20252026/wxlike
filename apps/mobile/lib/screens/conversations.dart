import 'package:flutter/material.dart';

import '../src/service.dart';
import '../src/widgets.dart';
import 'chat.dart';
import 'login.dart';

/// 会话列表。
///
/// 列表内容是 Rust 给的：`unread` 来自服务端信箱，`-1` 表示"没查到"（不是 0），
/// `pending` 是本会话队列里还没发出去的条数。离线时这一屏读的是本地缓存，
/// 所以飞行模式下打开也看得到入口。
class ConversationsScreen extends StatefulWidget {
  const ConversationsScreen({super.key, required this.service, required this.session});

  final WxlikeService service;
  final SessionInfo session;

  @override
  State<ConversationsScreen> createState() => _ConversationsScreenState();
}

class _ConversationsScreenState extends State<ConversationsScreen> {
  List<ConversationRow> _rows = const [];
  LiveInfo? _live;
  bool _fromCache = false;
  String? _error;
  bool _busy = true;

  @override
  void initState() {
    super.initState();
    _load();
  }

  Future<void> _load() async {
    setState(() {
      _busy = true;
      _error = null;
    });
    try {
      // connect 失败也要继续往下走：离线时列表读本地缓存
      try {
        await widget.service.connect();
      } on WxlikeErrorException catch (error) {
        _error = describeError(error);
      }
      final list = await widget.service.conversations();
      final live = await widget.service.liveInfo();
      if (!mounted) return;
      setState(() {
        _rows = list.conversations;
        _fromCache = list.fromCache;
        _live = live;
        _busy = false;
      });
    } catch (error) {
      if (!mounted) return;
      setState(() {
        _error = describeError(error);
        _busy = false;
      });
    }
  }

  Future<void> _logout() async {
    await widget.service.logout();
    if (!mounted) return;
    Navigator.of(context).pushAndRemoveUntil(
      MaterialPageRoute(builder: (_) => LoginScreen(service: widget.service)),
      (_) => false,
    );
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(
        title: Text('wxlike · ${widget.session.userId}'),
        actions: [
          IconButton(
            onPressed: _load,
            icon: const Icon(Icons.refresh),
            tooltip: '刷新',
          ),
          PopupMenuButton<String>(
            onSelected: (value) => value == 'logout' ? _logout() : null,
            itemBuilder: (_) => const [
              PopupMenuItem(value: 'logout', child: Text('退出登录')),
            ],
          ),
        ],
        bottom: _live == null
            ? null
            : PreferredSize(
                preferredSize: const Size.fromHeight(32),
                child: Padding(
                  padding: const EdgeInsets.only(left: 16, bottom: 8),
                  child: Align(
                    alignment: Alignment.centerLeft,
                    child: LiveChip(info: _live!),
                  ),
                ),
              ),
      ),
      body: RefreshIndicator(
        onRefresh: _load,
        child: _busy && _rows.isEmpty
            ? const Center(child: CircularProgressIndicator())
            : ListView(
                physics: const AlwaysScrollableScrollPhysics(),
                children: [
                  if (_fromCache)
                    const Padding(
                      padding: EdgeInsets.fromLTRB(16, 12, 16, 0),
                      // 列表是本地缓存的那一份：未读数可能是旧的，不能装作刚拉过
                      child: Text('离线中 · 下面是本地缓存的会话'),
                    ),
                  if (_error != null)
                    Padding(
                      padding: const EdgeInsets.all(12),
                      child: ErrorBox(message: _error!),
                    ),
                  if (_rows.isEmpty)
                    const Padding(
                      padding: EdgeInsets.fromLTRB(16, 48, 16, 16),
                      child: Center(child: Text('一个会话也没有\n（离线且本地没有缓存时也会这样）')),
                    ),
                  for (final row in _rows) _tile(row),
                ],
              ),
      ),
    );
  }

  Widget _tile(ConversationRow row) {
    final subtitle = row.kind == 'group' ? '群 · ${row.members} 人' : '直聊';
    return ListTile(
      title: Text(row.title),
      subtitle: Text([
        subtitle,
        if (row.pending > 0) '${row.pending} 条待发送',
      ].join(' · ')),
      trailing: row.unread < 0
          ? const Text('未读未知')
          : (row.unread == 0
              ? null
              : CircleAvatar(
                  radius: 12,
                  child: Text('${row.unread}', style: const TextStyle(fontSize: 12)),
                )),
      onTap: () async {
        await Navigator.of(context).push(
          MaterialPageRoute(
            builder: (_) => ChatScreen(service: widget.service, row: row),
          ),
        );
        // 从聊天回来时未读可能已经变了
        _load();
      },
    );
  }
}
