import 'package:flutter/material.dart';

import '../../shell/theme.dart';
import '../../shell/widgets.dart';
import 'contacts_controller.dart';
import 'models.dart';

/// 通讯录：好友分页、搜索、新朋友审批、群列表。
class ContactsPage extends StatefulWidget {
  const ContactsPage({
    super.key,
    required this.controller,
    required this.onOpenProfile,
    required this.onOpenGroup,
    required this.onOpenChat,
  });

  final ContactsController controller;
  final ValueChanged<String> onOpenProfile;
  final ValueChanged<String> onOpenGroup;
  final ValueChanged<Contact> onOpenChat;

  @override
  State<ContactsPage> createState() => _ContactsPageState();
}

class _ContactsPageState extends State<ContactsPage> {
  final _search = TextEditingController();
  final _scroll = ScrollController();

  @override
  void initState() {
    super.initState();
    widget.controller.addListener(_rebuild);
    _scroll.addListener(_onScroll);
    widget.controller.load();
  }

  void _rebuild() {
    if (mounted) setState(() {});
  }

  void _onScroll() {
    if (_scroll.position.extentAfter < 240) widget.controller.loadMore();
  }

  @override
  void dispose() {
    widget.controller.removeListener(_rebuild);
    _scroll.dispose();
    _search.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final controller = widget.controller;
    return Column(
      children: [
        _header(),
        _sections(controller),
        if (controller.section == ContactsSection.friends) _searchBar(controller),
        if (controller.section == ContactsSection.friends && controller.error != null)
          Padding(
            padding: const EdgeInsets.fromLTRB(12, 8, 12, 0),
            child: G12ErrorBanner(
              message: controller.error!.displayText,
              onRetry: () => controller.load(refresh: true),
            ),
          ),
        if (controller.section == ContactsSection.requests && controller.requestError != null)
          Padding(
            padding: const EdgeInsets.fromLTRB(12, 8, 12, 0),
            child: G12ErrorBanner(
              message: controller.requestError!.displayText,
              onRetry: controller.loadRequests,
            ),
          ),
        if (controller.section == ContactsSection.groups && controller.groupError != null)
          Padding(
            padding: const EdgeInsets.fromLTRB(12, 8, 12, 0),
            child: G12ErrorBanner(
              message: controller.groupError!.displayText,
              onRetry: controller.loadGroups,
            ),
          ),
        Expanded(child: _body(controller)),
      ],
    );
  }

  Widget _header() {
    return Container(
      padding: const EdgeInsets.fromLTRB(16, 8, 16, 4),
      alignment: Alignment.centerLeft,
      child: const Text('通讯录', style: TextStyle(fontSize: 18, fontWeight: FontWeight.w600)),
    );
  }

  Widget _sections(ContactsController controller) {
    Widget chip(ContactsSection section, String label, IconData icon) {
      final selected = controller.section == section;
      return Padding(
        padding: const EdgeInsets.only(right: 8),
        child: ChoiceChip(
          key: Key('contacts-section-${section.name}'),
          selected: selected,
          onSelected: (_) => controller.select(section),
          avatar: Icon(icon, size: 16, color: selected ? WxColors.brandDark : null),
          label: Text(label),
        ),
      );
    }

    return SingleChildScrollView(
      scrollDirection: Axis.horizontal,
      padding: const EdgeInsets.fromLTRB(12, 4, 12, 4),
      child: Row(
        children: [
          chip(ContactsSection.friends, '好友', Icons.people_outline),
          chip(ContactsSection.requests, '新朋友', Icons.person_add_alt_1_outlined),
          chip(ContactsSection.groups, '群聊', Icons.groups_outlined),
        ],
      ),
    );
  }

  Widget _searchBar(ContactsController controller) {
    return Padding(
      padding: const EdgeInsets.fromLTRB(12, 4, 12, 8),
      child: TextField(
        key: const Key('contacts-search'),
        controller: _search,
        onChanged: controller.search,
        decoration: InputDecoration(
          hintText: '搜索昵称、备注或标签',
          prefixIcon: const Icon(Icons.search, size: 18),
          suffixIcon: controller.query.isEmpty
              ? null
              : IconButton(
                  icon: const Icon(Icons.close, size: 16),
                  onPressed: () {
                    _search.clear();
                    controller.search('');
                  },
                ),
          isDense: true,
        ),
      ),
    );
  }

  Widget _body(ContactsController controller) {
    switch (controller.section) {
      case ContactsSection.friends:
        if (controller.loading && controller.contacts.isEmpty) {
          return const Center(child: CircularProgressIndicator());
        }
        if (controller.contacts.isEmpty) {
          return G12EmptyState(
            title: controller.query.isEmpty ? '还没有联系人' : '没有匹配的联系人',
            hint: controller.query.isEmpty ? '好友通过后会自动出现在这里' : '换个关键词，或清空搜索',
          );
        }
        return _friends(controller);
      case ContactsSection.requests:
        if (controller.loadingRequests && controller.requests.isEmpty) {
          return const Center(child: CircularProgressIndicator());
        }
        if (controller.requests.isEmpty) {
          return const G12EmptyState(title: '没有新的好友申请', hint: '申请被处理后也会保留在这里');
        }
        return ListView.separated(
          itemCount: controller.requests.length,
          separatorBuilder: (_, _) => const Divider(height: 1),
          itemBuilder: (context, index) => _requestTile(controller, controller.requests[index]),
        );
      case ContactsSection.groups:
        if (controller.loadingGroups && controller.groups.isEmpty) {
          return const Center(child: CircularProgressIndicator());
        }
        if (controller.groups.isEmpty) {
          return const G12EmptyState(title: '还没有群聊', hint: '被邀请或创建群后会显示在这里');
        }
        return ListView.separated(
          itemCount: controller.groups.length,
          separatorBuilder: (_, _) => const Divider(height: 1),
          itemBuilder: (context, index) {
            final group = controller.groups[index];
            return ListTile(
              leading: CircleAvatar(
                backgroundColor: WxColors.brandSoft,
                child: const Icon(Icons.groups, color: WxColors.brandDark, size: 20),
              ),
              title: Text(group.name),
              subtitle: Text('${group.memberCount} 人 · ${_roleLabel(group.role)}'),
              trailing: const Icon(Icons.chevron_right),
              onTap: () => widget.onOpenGroup(group.groupId),
            );
          },
        );
    }
  }

  Widget _friends(ContactsController controller) {
    final starred = controller.contacts.where((contact) => contact.starred).toList();
    final others = controller.contacts.where((contact) => !contact.starred).toList();
    final entries = <Object>[];
    if (starred.isNotEmpty) {
      entries.add('星标朋友');
      entries.addAll(starred);
    }
    String? currentLetter;
    for (final contact in others) {
      if (contact.indexLetter != currentLetter) {
        currentLetter = contact.indexLetter;
        entries.add(currentLetter);
      }
      entries.add(contact);
    }
    if (controller.hasMore) entries.add('more');
    return ListView.builder(
      controller: _scroll,
      itemCount: entries.length,
      itemBuilder: (context, index) {
        final entry = entries[index];
        if (entry == 'more') {
          return Padding(
            padding: const EdgeInsets.all(16),
            child: Center(
              child: controller.loadingMore
                  ? const SizedBox(width: 20, height: 20, child: CircularProgressIndicator(strokeWidth: 2))
                  : OutlinedButton(onPressed: controller.loadMore, child: const Text('加载更多')),
            ),
          );
        }
        if (entry is String) {
          return Container(
            color: Theme.of(context).brightness == Brightness.dark
                ? WxColors.darkGrouped
                : WxColors.bgGrouped,
            padding: const EdgeInsets.fromLTRB(16, 5, 16, 5),
            child: Text(entry, style: const TextStyle(fontSize: 12, color: WxColors.textSecondary)),
          );
        }
        final contact = entry as Contact;
        return ListTile(
          leading: CircleAvatar(
            backgroundColor: WxColors.brandSoft,
            child: Text(
              contact.title.substring(0, 1).toUpperCase(),
              style: const TextStyle(color: WxColors.brandDark, fontWeight: FontWeight.w600),
            ),
          ),
          title: Text(contact.title),
          subtitle: contact.tags.isEmpty ? null : Text(contact.tags.join(' · ')),
          trailing: contact.starred ? const Icon(Icons.star, size: 18, color: WxColors.warning) : null,
          onTap: () => widget.onOpenProfile(contact.userId),
          onLongPress: () => _contactSheet(contact),
        );
      },
    );
  }

  Widget _requestTile(ContactsController controller, FriendRequest request) {
    final busy = controller.deciding.contains(request.requestId);
    return ListTile(
      leading: CircleAvatar(
        backgroundColor: WxColors.brandSoft,
        child: Text(request.title.substring(0, 1).toUpperCase(), style: const TextStyle(color: WxColors.brandDark)),
      ),
      title: Text(request.title),
      subtitle: Text(
        request.message.isEmpty ? _statusLabel(request.status) : '${request.message} · ${_statusLabel(request.status)}',
        maxLines: 2,
        overflow: TextOverflow.ellipsis,
      ),
      trailing: request.pending
          ? Row(
              mainAxisSize: MainAxisSize.min,
              children: [
                IconButton(
                  key: Key('friend-reject-${request.requestId}'),
                  onPressed: busy ? null : () => controller.decide(request, accept: false),
                  icon: const Icon(Icons.close, size: 18),
                  tooltip: '拒绝',
                ),
                FilledButton(
                  key: Key('friend-accept-${request.requestId}'),
                  onPressed: busy ? null : () => controller.decide(request, accept: true),
                  child: busy
                      ? const SizedBox(width: 14, height: 14, child: CircularProgressIndicator(strokeWidth: 2))
                      : const Text('接受'),
                ),
              ],
            )
          : Text(_statusLabel(request.status), style: const TextStyle(fontSize: 12, color: WxColors.textSecondary)),
    );
  }

  Future<void> _contactSheet(Contact contact) async {
    final action = await showModalBottomSheet<String>(
      context: context,
      builder: (context) => SafeArea(
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            ListTile(
              leading: const Icon(Icons.chat_bubble_outline),
              title: const Text('发消息'),
              onTap: () => Navigator.pop(context, 'chat'),
            ),
            ListTile(
              leading: const Icon(Icons.person_outline),
              title: const Text('查看资料'),
              onTap: () => Navigator.pop(context, 'profile'),
            ),
          ],
        ),
      ),
    );
    if (!mounted) return;
    if (action == 'chat') widget.onOpenChat(contact);
    if (action == 'profile') widget.onOpenProfile(contact.userId);
  }

  static String _roleLabel(GroupRole role) => switch (role) {
        GroupRole.owner => '群主',
        GroupRole.admin => '管理员',
        GroupRole.member => '成员',
      };

  static String _statusLabel(FriendRequestStatus status) => switch (status) {
        FriendRequestStatus.pending => '待处理',
        FriendRequestStatus.accepted => '已通过',
        FriendRequestStatus.rejected => '已拒绝',
        FriendRequestStatus.cancelled => '已取消',
      };
}
