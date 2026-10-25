/// G13-06：我的状态与好友状态。
///
/// 发布内容、可见范围与有效期都由 `content.status.*` 处理；背景图来自平台桥
/// （拍摄/相册，G12-05），未接入时允许发布无背景状态并明确标注。好友状态的
/// 到期与否由服务端判定，本页只展示服务端返回的条目与到期时间。
library;

import 'package:flutter/material.dart';

import '../g13/core.dart';
import '../g13/models.dart';
import '../g13/ports.dart';
import '../g13/widgets.dart';

/// 选择一张背景图，返回 media_id 或服务端可识别的背景值；取消返回 null。
typedef StatusBackgroundPicker = Future<String?> Function();

const List<(String, String)> _visibilities = [
  ('friends', '好友可见'),
  ('close_friends', '部分好友'),
  ('private', '仅自己'),
];

const List<(int, String)> _ttls = [(1, '1 小时'), (6, '6 小时'), (24, '24 小时')];

String _visibilityLabel(String value) {
  for (final (code, label) in _visibilities) {
    if (code == value) return label;
  }
  return value;
}

class StatusPage extends StatefulWidget {
  const StatusPage({super.key, required this.ports, this.pickBackground});

  final G13Ports ports;
  final StatusBackgroundPicker? pickBackground;

  @override
  State<StatusPage> createState() => _StatusPageState();
}

class _StatusPageState extends State<StatusPage> {
  UserStatus? _mine;
  Object? _mineError;
  bool _mineLoading = true;
  bool _clearing = false;
  late Future<PageResult<UserStatus>> _friends = _loadFriends();

  @override
  void initState() {
    super.initState();
    _loadMine();
  }

  Future<void> _loadMine() async {
    setState(() {
      _mineLoading = true;
      _mineError = null;
    });
    try {
      final status = await widget.ports.status.myStatus();
      if (!mounted) return;
      setState(() => _mine = status);
    } catch (error) {
      if (mounted) setState(() => _mineError = error);
    } finally {
      if (mounted) setState(() => _mineLoading = false);
    }
  }

  Future<PageResult<UserStatus>> _loadFriends() =>
      widget.ports.status.friendStatuses();

  void _reload() {
    if (!mounted) return;
    setState(() {
      _friends = _loadFriends();
    });
    _loadMine();
  }

  Future<void> _compose() async {
    final published = await showModalBottomSheet<UserStatus>(
      context: context,
      isScrollControlled: true,
      showDragHandle: true,
      builder: (context) => Padding(
        padding: EdgeInsets.only(
          bottom: MediaQuery.of(context).viewInsets.bottom,
        ),
        child: _ComposeStatusSheet(
          ports: widget.ports,
          pickBackground: widget.pickBackground,
        ),
      ),
    );
    if (published == null || !mounted) return;
    setState(() => _mine = published);
    setState(() {
      _friends = _loadFriends();
    });
  }

  Future<void> _clear() async {
    if (_clearing) return;
    final status = _mine;
    if (status == null) return;
    final confirmed = await showDialog<bool>(
      context: context,
      builder: (context) => AlertDialog(
        title: const Text('取消我的状态'),
        content: const Text('取消后好友将不再看到当前状态。'),
        actions: [
          TextButton(
            onPressed: () => Navigator.of(context).pop(false),
            child: const Text('保留'),
          ),
          FilledButton(
            onPressed: () => Navigator.of(context).pop(true),
            child: const Text('取消状态'),
          ),
        ],
      ),
    );
    if (confirmed != true || !mounted) return;
    setState(() {
      _clearing = true;
      _mineError = null;
    });
    try {
      await widget.ports.status.clearStatus(
        idempotencyKey: 'status-clear-${status.id}',
      );
      if (!mounted) return;
      setState(() => _mine = null);
      setState(() {
        _friends = _loadFriends();
      });
    } catch (error) {
      if (mounted) setState(() => _mineError = error);
    } finally {
      if (mounted) setState(() => _clearing = false);
    }
  }

  @override
  Widget build(BuildContext context) {
    return G13Scaffold(
      title: '状态',
      onBack: () => Navigator.of(context).maybePop(),
      actions: [
        IconButton(onPressed: _reload, icon: const Icon(Icons.refresh)),
        IconButton(
          onPressed: _compose,
          icon: const Icon(Icons.add_circle_outline),
        ),
      ],
      body: RefreshIndicator(
        onRefresh: () async => _reload(),
        child: ListView(
          children: [
            if (_mineError != null)
              InlineFailure(
                error: _mineError!,
                onRetry:
                    _mineError is G13Failure &&
                        (_mineError! as G13Failure).retryable
                    ? _loadMine
                    : null,
              ),
            const SectionTitle(text: '我的状态'),
            if (_mineLoading)
              const Padding(
                padding: EdgeInsets.all(24),
                child: Center(child: CircularProgressIndicator()),
              )
            else if (_mine == null)
              EmptyView(
                text: '还没有发布状态',
                hint: '点击右上角发布一条状态',
                action: FilledButton.tonal(
                  onPressed: _compose,
                  child: const Text('发布状态'),
                ),
              )
            else
              Card(
                margin: const EdgeInsets.fromLTRB(16, 4, 16, 8),
                child: ListTile(
                  leading: const Icon(Icons.emoji_emotions_outlined),
                  title: Text(_mine!.text),
                  subtitle: Text(
                    '${_visibilityLabel(_mine!.visibility)} · 到期 ${formatTime(_mine!.expiresAt)}'
                    '${_mine!.background.isEmpty ? ' · 无背景' : ''}',
                  ),
                  trailing: TextButton(
                    onPressed: _clearing ? null : _clear,
                    child: const Text('取消状态'),
                  ),
                ),
              ),
            const Divider(height: 24),
            const SectionTitle(text: '好友状态'),
            AsyncView<PageResult<UserStatus>>(
              future: _friends,
              onRetry: _reload,
              isEmpty: (page) => page.isEmpty,
              emptyText: '好友还没有发布状态',
              builder: (context, page) => Column(
                children: [
                  for (final status in page.items)
                    ListTile(
                      leading: AvatarDot(label: status.displayName),
                      title: Text(status.displayName),
                      subtitle: Text(
                        '${status.text}\n到期 ${formatTime(status.expiresAt)} · '
                        '${_visibilityLabel(status.visibility)}',
                      ),
                      isThreeLine: true,
                    ),
                ],
              ),
            ),
            const SizedBox(height: 24),
          ],
        ),
      ),
    );
  }
}

class _ComposeStatusSheet extends StatefulWidget {
  const _ComposeStatusSheet({required this.ports, this.pickBackground});

  final G13Ports ports;
  final StatusBackgroundPicker? pickBackground;

  @override
  State<_ComposeStatusSheet> createState() => _ComposeStatusSheetState();
}

class _ComposeStatusSheetState extends State<_ComposeStatusSheet> {
  final TextEditingController _text = TextEditingController();
  String _background = '';
  String _visibility = 'friends';
  int _ttlHours = 24;
  bool _publishing = false;
  Object? _error;
  String _key = '';
  String _keySig = '';

  @override
  void dispose() {
    _text.dispose();
    super.dispose();
  }

  Future<void> _pickBackground() async {
    final picker = widget.pickBackground;
    if (picker == null) return;
    try {
      final value = await picker();
      if (!mounted) return;
      if (value == null || value.isEmpty) return;
      setState(() {
        _background = value;
        _key = '';
      });
    } catch (error) {
      if (mounted) setState(() => _error = error);
    }
  }

  Future<void> _publish() async {
    if (_publishing) return;
    final text = _text.text.trim();
    if (text.isEmpty) {
      setState(
        () => _error = const G13Failure(
          G13FailureKind.invalidArgument,
          'EMPTY_STATUS',
          '状态内容不能为空',
        ),
      );
      return;
    }
    if (text.runes.length > 100) {
      setState(
        () => _error = const G13Failure(
          G13FailureKind.invalidArgument,
          'STATUS_TOO_LONG',
          '状态内容最多 100 个字',
        ),
      );
      return;
    }
    final sig = '$text\n$_background\n$_visibility\n$_ttlHours';
    if (sig != _keySig) {
      _key = newIdempotencyKey();
      _keySig = sig;
    }
    final key = _key;
    setState(() {
      _publishing = true;
      _error = null;
    });
    try {
      final status = await widget.ports.status.publishStatus(
        text: text,
        background: _background,
        visibility: _visibility,
        ttl: Duration(hours: _ttlHours),
        idempotencyKey: key,
      );
      if (!mounted) return;
      Navigator.of(context).pop(status);
    } catch (error) {
      if (mounted) setState(() => _error = error);
    } finally {
      if (mounted) setState(() => _publishing = false);
    }
  }

  @override
  Widget build(BuildContext context) {
    return SafeArea(
      child: SingleChildScrollView(
        padding: const EdgeInsets.fromLTRB(20, 0, 20, 20),
        child: Column(
          mainAxisSize: MainAxisSize.min,
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Text('发布状态', style: Theme.of(context).textTheme.titleMedium),
            const SizedBox(height: 12),
            TextField(
              controller: _text,
              enabled: !_publishing,
              maxLength: 100,
              minLines: 2,
              maxLines: 4,
              decoration: const InputDecoration(
                labelText: '此刻的想法',
                border: OutlineInputBorder(),
              ),
              onChanged: (_) => _key = '',
            ),
            Row(
              children: [
                OutlinedButton.icon(
                  onPressed: _publishing ? null : _pickBackground,
                  icon: const Icon(Icons.image_outlined),
                  label: Text(
                    _background.isEmpty
                        ? (widget.pickBackground == null ? '背景未接入' : '选择背景')
                        : '背景已选择',
                  ),
                ),
                const SizedBox(width: 8),
                if (widget.pickBackground == null)
                  Expanded(
                    child: Text(
                      '拍摄/相册由平台桥提供（G12-05），当前仅发布无背景状态。',
                      style: Theme.of(context).textTheme.bodySmall,
                    ),
                  ),
              ],
            ),
            const SizedBox(height: 12),
            Wrap(
              spacing: 8,
              children: [
                for (final (code, label) in _visibilities)
                  ChoiceChip(
                    label: Text(label),
                    selected: _visibility == code,
                    onSelected: _publishing
                        ? null
                        : (_) => setState(() {
                            _visibility = code;
                            _key = '';
                          }),
                  ),
              ],
            ),
            const SizedBox(height: 8),
            Wrap(
              spacing: 8,
              children: [
                for (final (hours, label) in _ttls)
                  ChoiceChip(
                    label: Text(label),
                    selected: _ttlHours == hours,
                    onSelected: _publishing
                        ? null
                        : (_) => setState(() {
                            _ttlHours = hours;
                            _key = '';
                          }),
                  ),
              ],
            ),
            if (_error != null) ...[
              const SizedBox(height: 12),
              InlineFailure(
                error: _error!,
                onRetry:
                    _error is G13Failure && (_error! as G13Failure).retryable
                    ? _publish
                    : null,
              ),
            ],
            const SizedBox(height: 16),
            Align(
              alignment: Alignment.centerRight,
              child: FilledButton(
                onPressed: _publishing ? null : _publish,
                child: _publishing
                    ? const SizedBox(
                        width: 18,
                        height: 18,
                        child: CircularProgressIndicator(strokeWidth: 2),
                      )
                    : const Text('发布'),
              ),
            ),
          ],
        ),
      ),
    );
  }
}
