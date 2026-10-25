import 'package:flutter/material.dart';

import '../../shell/contracts.dart';
import '../../shell/theme.dart';
import '../../shell/widgets.dart';
import 'models.dart';
import 'profile_controller.dart';
import 'relations_ports.dart';

/// 群资料与成员管理。角色动作由服务端判定；越权时显示业务拒绝而不是隐藏按钮。
class GroupPage extends StatefulWidget {
  const GroupPage({super.key, required this.port, required this.groupId, this.onOpenProfile});

  final RelationsPort port;
  final String groupId;
  final ValueChanged<String>? onOpenProfile;

  @override
  State<GroupPage> createState() => _GroupPageState();
}

class _GroupPageState extends State<GroupPage> {
  late final GroupController _controller = GroupController(port: widget.port, groupId: widget.groupId);

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
    final detail = _controller.detail;
    return Scaffold(
      appBar: AppBar(
        title: Text(detail?.summary.name ?? '群聊'),
        actions: [
          IconButton(onPressed: _controller.load, icon: const Icon(Icons.refresh), tooltip: '刷新'),
        ],
      ),
      body: _controller.loading && detail == null
          ? const Center(child: CircularProgressIndicator())
          : detail == null
              ? G12EmptyState(
                  title: '群资料加载失败',
                  hint: _controller.error?.displayText ?? '可以重新加载',
                  action: OutlinedButton(onPressed: _controller.load, child: const Text('重试')),
                )
              : ListView(
                  children: [
                    _header(detail),
                    if (_controller.error != null)
                      Padding(
                        padding: const EdgeInsets.fromLTRB(16, 12, 16, 0),
                        child: G12ErrorBanner(
                          message: _controller.error!.displayText,
                          onRetry: _controller.error!.kind == G12FailureKind.conflict ? _controller.load : null,
                        ),
                      ),
                    const SizedBox(height: 8),
                    _members(detail),
                    const SizedBox(height: 20),
                    Padding(
                      padding: const EdgeInsets.symmetric(horizontal: 16),
                      child: OutlinedButton(
                        onPressed: _controller.mutating ? null : _leave,
                        style: OutlinedButton.styleFrom(foregroundColor: WxColors.danger),
                        child: const Text('退出群聊'),
                      ),
                    ),
                    const SizedBox(height: 24),
                  ],
                ),
    );
  }

  Widget _header(GroupDetail detail) {
    return Container(
      color: Theme.of(context).brightness == Brightness.dark ? WxColors.darkSurface : WxColors.surface,
      padding: const EdgeInsets.fromLTRB(16, 20, 16, 16),
      child: Row(
        children: [
          CircleAvatar(
            radius: 26,
            backgroundColor: WxColors.brandSoft,
            child: const Icon(Icons.groups, color: WxColors.brandDark, size: 26),
          ),
          const SizedBox(width: 14),
          Expanded(
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Text(detail.summary.name, style: const TextStyle(fontSize: 17, fontWeight: FontWeight.w600)),
                Text(
                  '${detail.members.length} 人 · 我的角色：${_roleLabel(detail.myRole)}',
                  style: const TextStyle(fontSize: 12, color: WxColors.textSecondary),
                ),
              ],
            ),
          ),
          if (detail.canManage && detail.summary.role == GroupRole.owner)
            TextButton(onPressed: _addMembers, child: const Text('邀请')),
        ],
      ),
    );
  }

  Widget _members(GroupDetail detail) {
    return Container(
      color: Theme.of(context).brightness == Brightness.dark ? WxColors.darkSurface : WxColors.surface,
      child: Column(
        children: [
          for (final member in detail.members)
            ListTile(
              leading: CircleAvatar(
                backgroundColor: WxColors.brandSoft,
                child: Text(
                  member.title.substring(0, 1).toUpperCase(),
                  style: const TextStyle(color: WxColors.brandDark, fontWeight: FontWeight.w600),
                ),
              ),
              title: Text(member.title),
              subtitle: Text(_roleLabel(member.role)),
              onTap: widget.onOpenProfile == null ? null : () => widget.onOpenProfile!(member.userId),
              trailing: detail.canManage && member.role != GroupRole.owner
                  ? PopupMenuButton<String>(
                      enabled: !_controller.mutating,
                      onSelected: (value) => _memberAction(member, value),
                      itemBuilder: (context) => [
                        if (member.role == GroupRole.member)
                          const PopupMenuItem(value: 'promote', child: Text('设为管理员')),
                        if (member.role == GroupRole.admin)
                          const PopupMenuItem(value: 'demote', child: Text('取消管理员')),
                        const PopupMenuItem(value: 'kick', child: Text('移出群聊')),
                      ],
                    )
                  : null,
            ),
        ],
      ),
    );
  }

  Future<void> _memberAction(GroupMember member, String action) async {
    final ok = switch (action) {
      'promote' => await _controller.changeRole(member.userId, GroupRole.admin),
      'demote' => await _controller.changeRole(member.userId, GroupRole.member),
      'kick' => await _controller.kick(member.userId),
      _ => false,
    };
    if (!ok && mounted) {
      ScaffoldMessenger.of(context).showSnackBar(
        SnackBar(content: Text(_controller.error?.displayText ?? '操作未完成')),
      );
    }
  }

  Future<void> _addMembers() async {
    final input = TextEditingController();
    final value = await showDialog<String>(
      context: context,
      builder: (context) => AlertDialog(
        title: const Text('邀请成员'),
        content: TextField(
          key: const Key('group-add-members'),
          controller: input,
          decoration: const InputDecoration(hintText: '用户 ID，多个用逗号分隔'),
        ),
        actions: [
          TextButton(onPressed: () => Navigator.pop(context), child: const Text('取消')),
          FilledButton(onPressed: () => Navigator.pop(context, input.text.trim()), child: const Text('邀请')),
        ],
      ),
    );
    input.dispose();
    if (value == null || value.isEmpty) return;
    final ids = value
        .split(RegExp(r'[,，]'))
        .map((id) => id.trim())
        .where((id) => id.isNotEmpty)
        .toList();
    final ok = await _controller.addMembers(ids);
    if (!ok && mounted) {
      ScaffoldMessenger.of(context).showSnackBar(
        SnackBar(content: Text(_controller.error?.displayText ?? '邀请未完成')),
      );
    }
  }

  Future<void> _leave() async {
    final confirmed = await showDialog<bool>(
      context: context,
      builder: (context) => AlertDialog(
        title: const Text('退出群聊？'),
        content: const Text('退出后不再接收该群消息；群主需要先转让或解散。'),
        actions: [
          TextButton(onPressed: () => Navigator.pop(context, false), child: const Text('取消')),
          FilledButton(onPressed: () => Navigator.pop(context, true), child: const Text('退出')),
        ],
      ),
    );
    if (confirmed != true) return;
    final ok = await _controller.leave();
    if (!mounted) return;
    if (ok) {
      Navigator.of(context).pop();
    } else {
      ScaffoldMessenger.of(context).showSnackBar(
        SnackBar(content: Text(_controller.error?.displayText ?? '退出未完成')),
      );
    }
  }

  static String _roleLabel(GroupRole role) => switch (role) {
        GroupRole.owner => '群主',
        GroupRole.admin => '管理员',
        GroupRole.member => '成员',
      };
}
