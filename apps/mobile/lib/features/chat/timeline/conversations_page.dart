import 'package:flutter/material.dart';

import '../../../shell/theme.dart';
import '../../../shell/widgets.dart';
import '../../../src/service.dart';
import '../composer/draft_store.dart';
import 'conversations_controller.dart';

class ConversationsPage extends StatefulWidget {
  const ConversationsPage({
    super.key,
    required this.controller,
    required this.drafts,
    required this.onOpenChat,
    this.onOpenNewFriends,
    this.onOpenScan,
  });

  final ConversationsController controller;
  final DraftStore drafts;
  final ValueChanged<ConversationRow> onOpenChat;
  final VoidCallback? onOpenNewFriends;
  final VoidCallback? onOpenScan;

  @override
  State<ConversationsPage> createState() => _ConversationsPageState();
}

class _ConversationsPageState extends State<ConversationsPage> {
  final _search = TextEditingController();
  bool _searching = false;

  @override
  void initState() {
    super.initState();
    widget.controller.addListener(_rebuild);
    widget.drafts.addListener(_rebuild);
    widget.controller.load();
  }

  void _rebuild() {
    if (mounted) setState(() {});
  }

  @override
  void dispose() {
    widget.controller.removeListener(_rebuild);
    widget.drafts.removeListener(_rebuild);
    _search.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final controller = widget.controller;
    return Column(
      children: [
        _header(controller),
        if (_searching)
          Padding(
            padding: const EdgeInsets.fromLTRB(12, 0, 12, 8),
            child: TextField(
              key: const Key('chats-search'),
              controller: _search,
              autofocus: true,
              onChanged: controller.setQuery,
              decoration: InputDecoration(
                hintText: '筛选已加载的会话',
                prefixIcon: const Icon(Icons.search, size: 18),
                suffixIcon: IconButton(
                  icon: const Icon(Icons.close, size: 16),
                  onPressed: () {
                    _search.clear();
                    controller.setQuery('');
                    setState(() => _searching = false);
                  },
                ),
              ),
            ),
          ),
        if (controller.fromCache)
          Container(
            width: double.infinity,
            color: WxColors.warning.withValues(alpha: 0.15),
            padding: const EdgeInsets.symmetric(vertical: 4),
            child: const Text('离线 · 下面是本地缓存的会话', textAlign: TextAlign.center, style: TextStyle(fontSize: 11)),
          ),
        if (controller.error != null)
          Padding(
            padding: const EdgeInsets.fromLTRB(12, 8, 12, 0),
            child: G12ErrorBanner(
              message: controller.error!.displayText,
              onRetry: controller.load,
            ),
          ),
        Expanded(child: _list(controller)),
      ],
    );
  }

  Widget _header(ConversationsController controller) {
    final live = controller.live;
    return Container(
      color: Theme.of(context).appBarTheme.backgroundColor,
      padding: const EdgeInsets.fromLTRB(12, 4, 8, 6),
      child: Row(
        children: [
          const SizedBox(width: 4),
          const Text('微信', style: TextStyle(fontSize: 18, fontWeight: FontWeight.w600)),
          if (live != null) ...[
            const SizedBox(width: 8),
            _LiveDot(live: live),
          ],
          const Spacer(),
          IconButton(
            onPressed: () => setState(() => _searching = !_searching),
            icon: const Icon(Icons.search, size: 21),
            tooltip: '搜索',
          ),
          IconButton(
            onPressed: _showStartSheet,
            icon: const Icon(Icons.add, size: 23),
            tooltip: '发起',
          ),
        ],
      ),
    );
  }

  Widget _list(ConversationsController controller) {
    if (controller.busy && controller.rows.isEmpty) {
      return const Center(child: CircularProgressIndicator());
    }
    final rows = controller.visibleRows;
    return RefreshIndicator(
      onRefresh: controller.load,
      child: rows.isEmpty
          ? ListView(
              physics: const AlwaysScrollableScrollPhysics(),
              children: [
                controller.query.isNotEmpty
                    ? const G12EmptyState(title: '没有匹配的会话', hint: '只筛选已经加载的条目')
                    : G12EmptyState(
                        title: controller.error == null ? '还没有会话' : '会话未能加载',
                        hint: controller.error?.displayText,
                      ),
              ],
            )
          : ListView.separated(
              physics: const AlwaysScrollableScrollPhysics(),
              itemCount: rows.length,
              separatorBuilder: (_, _) => const Divider(height: 1, indent: 68),
              itemBuilder: (context, index) => _tile(rows[index]),
            ),
    );
  }

  Widget _tile(ConversationRow row) {
    final draft = widget.drafts.read(row.conv);
    final scheme = Theme.of(context).colorScheme;
    return InkWell(
      key: Key('conv-${row.conv}'),
      onTap: () {
        widget.onOpenChat(row);
        widget.controller.setForeground(true);
      },
      child: Padding(
        padding: const EdgeInsets.fromLTRB(12, 9, 12, 9),
        child: Row(
          children: [
            Container(
              width: 46,
              height: 46,
              decoration: BoxDecoration(
                color: row.kind == 'group'
                    ? const Color(0xff67A3E8)
                    : WxColors.brand.withValues(alpha: 0.85),
                borderRadius: BorderRadius.circular(6),
              ),
              child: Icon(
                row.kind == 'group' ? Icons.groups : Icons.person,
                color: Colors.white,
              ),
            ),
            const SizedBox(width: 10),
            Expanded(
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  Row(
                    children: [
                      Expanded(
                        child: Text(
                          row.title,
                          maxLines: 1,
                          overflow: TextOverflow.ellipsis,
                          style: const TextStyle(fontSize: 14),
                        ),
                      ),
                      if (row.unread > 0)
                        Container(
                          constraints: const BoxConstraints(minWidth: 17),
                          padding: const EdgeInsets.symmetric(horizontal: 5, vertical: 1),
                          decoration: const BoxDecoration(
                            color: WxColors.danger,
                            borderRadius: BorderRadius.all(Radius.circular(9)),
                          ),
                          child: Text(
                            row.unread > 99 ? '99+' : '${row.unread}',
                            textAlign: TextAlign.center,
                            style: const TextStyle(fontSize: 9, color: Colors.white),
                          ),
                        ),
                    ],
                  ),
                  const SizedBox(height: 3),
                  Text(
                    [
                      if (row.kind == 'group') '群 · ${row.members} 人' else '直聊',
                      if (draft.trim().isNotEmpty) '[草稿] ${draft.trim()}',
                      if (row.pending > 0) '${row.pending} 条待发送',
                    ].join(' · '),
                    maxLines: 1,
                    overflow: TextOverflow.ellipsis,
                    style: TextStyle(
                      fontSize: 11,
                      color: draft.trim().isNotEmpty ? WxColors.danger : scheme.outline,
                    ),
                  ),
                ],
              ),
            ),
          ],
        ),
      ),
    );
  }

  void _showStartSheet() {
    showModalBottomSheet<void>(
      context: context,
      showDragHandle: true,
      builder: (context) => SafeArea(
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            ListTile(
              leading: const Icon(Icons.person_add_alt),
              title: const Text('新的朋友'),
              onTap: () {
                Navigator.pop(context);
                widget.onOpenNewFriends?.call();
              },
            ),
            ListTile(
              leading: const Icon(Icons.groups),
              title: const Text('发起群聊'),
              subtitle: const Text('群创建由 G02 关系服务提供'),
              onTap: () {
                Navigator.pop(context);
                ScaffoldMessenger.of(context).showSnackBar(
                  const SnackBar(content: Text('群创建需 relations.create_group（G02），尚未接线')),
                );
              },
            ),
            ListTile(
              leading: const Icon(Icons.qr_code_scanner),
              title: const Text('扫一扫'),
              subtitle: const Text('扫码由 G07 提供'),
              onTap: () {
                Navigator.pop(context);
                final open = widget.onOpenScan;
                if (open != null) {
                  open();
                } else {
                  ScaffoldMessenger.of(context).showSnackBar(
                    const SnackBar(content: Text('扫一扫由 G07 接入，本页未挂载入口')),
                  );
                }
              },
            ),
          ],
        ),
      ),
    );
  }
}

class _LiveDot extends StatelessWidget {
  const _LiveDot({required this.live});

  final LiveInfo live;

  @override
  Widget build(BuildContext context) {
    final color = switch (live.state) {
      LiveState.online => WxColors.brand,
      LiveState.offline => WxColors.warning,
      LiveState.signedOut => Colors.blueGrey,
    };
    final label = switch (live.state) {
      LiveState.online => '在线',
      LiveState.offline => '离线',
      LiveState.signedOut => '未登录',
    };
    return Row(
      mainAxisSize: MainAxisSize.min,
      children: [
        Icon(Icons.circle, size: 8, color: color),
        const SizedBox(width: 4),
        Text(label, style: const TextStyle(fontSize: 10, color: WxColors.textSecondary)),
      ],
    );
  }
}
