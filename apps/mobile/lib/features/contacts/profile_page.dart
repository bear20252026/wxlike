import 'package:flutter/material.dart';

import '../../shell/contracts.dart';
import '../../shell/theme.dart';
import '../../shell/widgets.dart';
import 'models.dart';
import 'profile_controller.dart';
import 'relations_ports.dart';

/// 资料页：自己与好友共用布局，动作按 `isSelf`/`isFriend`/`blocked` 区分。
class ContactProfilePage extends StatefulWidget {
  const ContactProfilePage({
    super.key,
    required this.port,
    required this.userId,
    this.onOpenChat,
  });

  final RelationsPort port;
  final String userId;
  final void Function(String userId, String title)? onOpenChat;

  @override
  State<ContactProfilePage> createState() => _ContactProfilePageState();
}

class _ContactProfilePageState extends State<ContactProfilePage> {
  late final ProfileController _controller = ProfileController(port: widget.port, userId: widget.userId);

  @override
  void initState() {
    super.initState();
    _controller.addListener(_rebuild);
    _controller.load();
  }

  void _rebuild() {
    if (mounted) setState(() {});
  }

  @override
  void dispose() {
    _controller.removeListener(_rebuild);
    _controller.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final profile = _controller.profile;
    return Scaffold(
      appBar: AppBar(
        title: Text(profile?.isSelf == true ? '我的资料' : '详细资料'),
        actions: [
          if (profile != null && !profile.isSelf)
            PopupMenuButton<String>(
              onSelected: _onMenu,
              itemBuilder: (context) => [
                if (profile.isFriend)
                  const PopupMenuItem(value: 'remove', child: Text('删除好友')),
                if (profile.blocked)
                  const PopupMenuItem(value: 'unblock', child: Text('解除拉黑'))
                else
                  const PopupMenuItem(value: 'block', child: Text('加入黑名单')),
              ],
            ),
        ],
      ),
      body: _controller.loading && profile == null
          ? const Center(child: CircularProgressIndicator())
          : profile == null
              ? G12EmptyState(
                  title: '资料加载失败',
                  hint: _controller.error?.displayText ?? '可以重新加载',
                  action: OutlinedButton(onPressed: _controller.load, child: const Text('重试')),
                )
              : ListView(
                  children: [
                    _header(profile),
                    if (_controller.error != null)
                      Padding(
                        padding: const EdgeInsets.fromLTRB(16, 12, 16, 0),
                        child: G12ErrorBanner(
                          message: _controller.error!.displayText,
                          onRetry: _controller.error!.kind == G12FailureKind.conflict ? _controller.load : null,
                        ),
                      ),
                    if (!profile.isSelf) ...[
                      const SizedBox(height: 12),
                      _actions(profile),
                      const SizedBox(height: 12),
                      _prefs(profile),
                    ],
                    if (profile.commonGroups.isNotEmpty) ...[
                      const SizedBox(height: 12),
                      _commonGroups(profile),
                    ],
                  ],
                ),
    );
  }

  Widget _header(ContactProfile profile) {
    return Container(
      color: Theme.of(context).brightness == Brightness.dark ? WxColors.darkSurface : WxColors.surface,
      padding: const EdgeInsets.fromLTRB(16, 20, 16, 20),
      child: Row(
        children: [
          CircleAvatar(
            radius: 30,
            backgroundColor: WxColors.brandSoft,
            child: Text(
              profile.title.substring(0, 1).toUpperCase(),
              style: const TextStyle(fontSize: 22, color: WxColors.brandDark, fontWeight: FontWeight.w600),
            ),
          ),
          const SizedBox(width: 14),
          Expanded(
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Text(profile.title, style: const TextStyle(fontSize: 18, fontWeight: FontWeight.w600)),
                if (profile.remark.isNotEmpty && profile.remark != profile.displayName)
                  Text('昵称：${profile.displayName}', style: const TextStyle(fontSize: 12, color: WxColors.textSecondary)),
                Text('ID：${profile.userId}', style: const TextStyle(fontSize: 12, color: WxColors.textSecondary)),
                if (profile.bio.isNotEmpty) ...[
                  const SizedBox(height: 4),
                  Text(profile.bio, style: const TextStyle(fontSize: 13)),
                ],
              ],
            ),
          ),
        ],
      ),
    );
  }

  Widget _actions(ContactProfile profile) {
    return Container(
      color: Theme.of(context).brightness == Brightness.dark ? WxColors.darkSurface : WxColors.surface,
      padding: const EdgeInsets.symmetric(vertical: 8),
      child: Row(
        children: [
          Expanded(
            child: TextButton.icon(
              onPressed: profile.isFriend && !profile.blocked
                  ? () => widget.onOpenChat?.call(profile.userId, profile.title)
                  : null,
              icon: const Icon(Icons.chat_bubble_outline, size: 18),
              label: const Text('发消息'),
            ),
          ),
          Expanded(
            child: TextButton.icon(
              onPressed: _controller.saving ? null : _controller.toggleStar,
              icon: Icon(profile.starred ? Icons.star : Icons.star_border, size: 18, color: WxColors.warning),
              label: Text(profile.starred ? '取消星标' : '设为星标'),
            ),
          ),
        ],
      ),
    );
  }

  Widget _prefs(ContactProfile profile) {
    return Container(
      color: Theme.of(context).brightness == Brightness.dark ? WxColors.darkSurface : WxColors.surface,
      child: Column(
        children: [
          ListTile(
            title: const Text('备注'),
            subtitle: Text(profile.remark.isEmpty ? '未设置' : profile.remark),
            trailing: const Icon(Icons.chevron_right),
            onTap: _controller.saving ? null : _editRemark,
          ),
          ListTile(
            title: const Text('标签'),
            subtitle: Text(profile.tags.isEmpty ? '未设置' : profile.tags.join(' · ')),
            trailing: const Icon(Icons.chevron_right),
            onTap: _controller.saving ? null : _editTags,
          ),
          ListTile(
            title: Text(profile.blocked ? '解除拉黑' : '加入黑名单'),
            subtitle: const Text('拉黑后双方无法发消息、申请或通话音；服务端会再次判定'),
            trailing: _controller.saving
                ? const SizedBox(width: 18, height: 18, child: CircularProgressIndicator(strokeWidth: 2))
                : const Icon(Icons.chevron_right),
            onTap: _controller.saving ? null : (profile.blocked ? _controller.unblock : _controller.block),
          ),
        ],
      ),
    );
  }

  Widget _commonGroups(ContactProfile profile) {
    return Container(
      color: Theme.of(context).brightness == Brightness.dark ? WxColors.darkSurface : WxColors.surface,
      padding: const EdgeInsets.symmetric(vertical: 8),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          const Padding(
            padding: EdgeInsets.fromLTRB(16, 4, 16, 4),
            child: Text('共同群聊', style: TextStyle(fontSize: 13, color: WxColors.textSecondary)),
          ),
          for (final group in profile.commonGroups)
            ListTile(dense: true, leading: const Icon(Icons.groups_outlined), title: Text(group)),
        ],
      ),
    );
  }

  Future<void> _editRemark() async {
    final current = _controller.profile;
    if (current == null) return;
    final input = TextEditingController(text: current.remark);
    final value = await showDialog<String>(
      context: context,
      builder: (context) => AlertDialog(
        title: const Text('设置备注'),
        content: TextField(
          key: const Key('profile-remark'),
          controller: input,
          maxLength: 64,
          decoration: const InputDecoration(hintText: '仅自己可见'),
        ),
        actions: [
          TextButton(onPressed: () => Navigator.pop(context), child: const Text('取消')),
          FilledButton(onPressed: () => Navigator.pop(context, input.text.trim()), child: const Text('保存')),
        ],
      ),
    );
    input.dispose();
    if (value == null || value == current.remark) return;
    await _controller.savePrefs(remark: value);
  }

  Future<void> _editTags() async {
    final current = _controller.profile;
    if (current == null) return;
    final input = TextEditingController(text: current.tags.join(','));
    final value = await showDialog<String>(
      context: context,
      builder: (context) => AlertDialog(
        title: const Text('设置标签'),
        content: TextField(
          key: const Key('profile-tags'),
          controller: input,
          decoration: const InputDecoration(hintText: '用逗号分隔，最多 8 个'),
        ),
        actions: [
          TextButton(onPressed: () => Navigator.pop(context), child: const Text('取消')),
          FilledButton(
            onPressed: () {
              final tags = input.text
                  .split(RegExp(r'[,，]'))
                  .map((tag) => tag.trim())
                  .where((tag) => tag.isNotEmpty)
                  .toList();
              if (tags.length > 8 || tags.any((tag) => tag.length > 64)) {
                ScaffoldMessenger.of(context).showSnackBar(
                  const SnackBar(content: Text('最多 8 个标签，每个不超过 64 字')),
                );
                return;
              }
              Navigator.pop(context, tags.join(','));
            },
            child: const Text('保存'),
          ),
        ],
      ),
    );
    input.dispose();
    if (value == null) return;
    final tags = value.isEmpty ? <String>[] : value.split(',');
    await _controller.savePrefs(tags: tags);
  }

  Future<void> _onMenu(String action) async {
    switch (action) {
      case 'block':
        await _controller.block();
      case 'unblock':
        await _controller.unblock();
      case 'remove':
        final confirmed = await showDialog<bool>(
          context: context,
          builder: (context) => AlertDialog(
            title: const Text('删除好友？'),
            content: const Text('删除后聊天记录仍保留，但双方不再是好友。'),
            actions: [
              TextButton(onPressed: () => Navigator.pop(context, false), child: const Text('取消')),
              FilledButton(onPressed: () => Navigator.pop(context, true), child: const Text('删除')),
            ],
          ),
        );
        if (confirmed == true) {
          try {
            await widget.port.removeFriend(widget.userId, idempotencyKey: newIdempotencyKey('friend-remove'));
            await _controller.load();
          } catch (error) {
            if (!mounted) return;
            ScaffoldMessenger.of(context).showSnackBar(
              SnackBar(content: Text(asG12Failure(error, operation: '删除好友').displayText)),
            );
          }
        }
    }
    if (!mounted) return;
    final failure = _controller.error;
    if (failure != null) {
      ScaffoldMessenger.of(context).showSnackBar(SnackBar(content: Text(failure.displayText)));
    }
  }
}
