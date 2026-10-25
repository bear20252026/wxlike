/// G13-05：我的资料。
///
/// 昵称/签名更新带 `expected_version`，并发修改返回冲突时提示刷新后再改；
/// 头像上传依赖媒体服务与平台桥（G04/G12），未接线时只展示 media_id，不画假头像。
library;

import 'package:flutter/material.dart';

import '../content/g13/core.dart';
import '../content/g13/models.dart';
import '../content/g13/ports.dart';
import '../content/g13/widgets.dart';
import '../content/status/status_page.dart';
import '../discover/mycode/my_code_page.dart';

class ProfilePage extends StatefulWidget {
  const ProfilePage({super.key, required this.ports});

  final G13Ports ports;

  @override
  State<ProfilePage> createState() => _ProfilePageState();
}

class _ProfilePageState extends State<ProfilePage> {
  final TextEditingController _name = TextEditingController();
  final TextEditingController _signature = TextEditingController();
  late Future<ProfileInfo> _future = _load();
  ProfileInfo? _profile;
  bool _editing = false;
  bool _saving = false;
  Object? _error;
  String _key = '';
  String _keySig = '';

  @override
  void dispose() {
    _name.dispose();
    _signature.dispose();
    super.dispose();
  }

  Future<ProfileInfo> _load() async {
    try {
      final profile = await widget.ports.profile.profile();
      if (mounted) {
        setState(() {
          _profile = profile;
          if (!_editing) {
            _name.text = profile.name;
            _signature.text = profile.signature;
          }
        });
      }
      return profile;
    } catch (error) {
      if (mounted) setState(() => _error = error);
      rethrow;
    }
  }

  void _reload() {
    if (!mounted) return;
    setState(() {
      _error = null;
      _key = '';
      _keySig = '';
      _future = _load();
    });
  }

  void _startEdit(ProfileInfo profile) {
    _name.text = profile.name;
    _signature.text = profile.signature;
    setState(() {
      _editing = true;
      _error = null;
    });
  }

  Future<void> _save() async {
    if (_saving) return;
    final profile = _profile;
    if (profile == null) return;
    final name = _name.text.trim();
    final signature = _signature.text.trim();
    if (name.isEmpty) {
      setState(
        () => _error = const G13Failure(
          G13FailureKind.invalidArgument,
          'EMPTY_NAME',
          '昵称不能为空',
        ),
      );
      return;
    }
    if (name.runes.length > 32) {
      setState(
        () => _error = const G13Failure(
          G13FailureKind.invalidArgument,
          'NAME_TOO_LONG',
          '昵称最多 32 个字',
        ),
      );
      return;
    }
    if (signature.runes.length > 200) {
      setState(
        () => _error = const G13Failure(
          G13FailureKind.invalidArgument,
          'SIGNATURE_TOO_LONG',
          '个性签名最多 200 个字',
        ),
      );
      return;
    }
    final sig = '$name\n$signature\n${profile.version}';
    if (sig != _keySig) {
      _key = newIdempotencyKey();
      _keySig = sig;
    }
    final key = _key;
    setState(() {
      _saving = true;
      _error = null;
    });
    try {
      final updated = await widget.ports.profile.updateProfile(
        name: name,
        signature: signature,
        expectedVersion: profile.version,
        idempotencyKey: key,
      );
      if (!mounted) return;
      setState(() {
        _profile = updated;
        _editing = false;
        _key = '';
        _keySig = '';
      });
    } catch (error) {
      if (mounted) setState(() => _error = error);
    } finally {
      if (mounted) setState(() => _saving = false);
    }
  }

  @override
  Widget build(BuildContext context) {
    return G13Scaffold(
      title: '我的资料',
      onBack: () => Navigator.of(context).maybePop(),
      actions: [
        IconButton(onPressed: _reload, icon: const Icon(Icons.refresh)),
      ],
      body: AsyncView<ProfileInfo>(
        future: _future,
        onRetry: _reload,
        builder: (context, loaded) => _body(_profile ?? loaded),
      ),
    );
  }

  Widget _body(ProfileInfo profile) {
    final error = _error;
    return ListView(
      children: [
        const SizedBox(height: 16),
        Center(
          child: Column(
            children: [
              AvatarDot(label: profile.name.isEmpty ? '?' : profile.name),
              const SizedBox(height: 8),
              Text(
                profile.name,
                style: Theme.of(context).textTheme.titleMedium,
              ),
              Text(
                profile.userId,
                style: Theme.of(context).textTheme.bodySmall,
              ),
            ],
          ),
        ),
        if (error != null) ...[
          const SizedBox(height: 12),
          InlineFailure(
            error: error,
            onRetry: error is G13Failure && error.retryable
                ? () {
                    setState(() => _error = null);
                    _save();
                  }
                : null,
          ),
          if (error is G13Failure && error.kind == G13FailureKind.conflict)
            Align(
              alignment: Alignment.centerRight,
              child: TextButton(onPressed: _reload, child: const Text('刷新资料')),
            ),
        ],
        if (!_editing) ...[
          const SizedBox(height: 8),
          _readOnly('昵称', profile.name),
          _readOnly(
            '个性签名',
            profile.signature.isEmpty ? '未填写' : profile.signature,
          ),
          _readOnly('地区', profile.region.isEmpty ? '未设置' : profile.region),
          _readOnly(
            '头像',
            profile.avatarMediaId.isEmpty
                ? '尚未设置'
                : '头像由媒体服务提供（G04），当前未渲染 media_id：${profile.avatarMediaId}',
          ),
          const SizedBox(height: 8),
          Padding(
            padding: const EdgeInsets.symmetric(horizontal: 16),
            child: FilledButton.tonal(
              onPressed: () => _startEdit(profile),
              child: const Text('编辑资料'),
            ),
          ),
          const Divider(height: 32),
          SettingsTile(
            icon: Icons.qr_code_2,
            title: '我的二维码',
            onTap: () => Navigator.of(context).push(
              MaterialPageRoute<void>(
                builder: (_) => MyCodePage(ports: widget.ports),
              ),
            ),
          ),
          SettingsTile(
            icon: Icons.emoji_emotions_outlined,
            title: '我的状态',
            onTap: () => Navigator.of(context).push(
              MaterialPageRoute<void>(
                builder: (_) => StatusPage(ports: widget.ports),
              ),
            ),
          ),
        ] else ...[
          const SizedBox(height: 16),
          Padding(
            padding: const EdgeInsets.symmetric(horizontal: 16),
            child: TextField(
              controller: _name,
              enabled: !_saving,
              maxLength: 32,
              decoration: const InputDecoration(
                labelText: '昵称',
                border: OutlineInputBorder(),
              ),
            ),
          ),
          Padding(
            padding: const EdgeInsets.symmetric(horizontal: 16),
            child: TextField(
              controller: _signature,
              enabled: !_saving,
              maxLength: 200,
              minLines: 2,
              maxLines: 5,
              decoration: const InputDecoration(
                labelText: '个性签名',
                border: OutlineInputBorder(),
              ),
            ),
          ),
          Padding(
            padding: const EdgeInsets.symmetric(horizontal: 16),
            child: Row(
              mainAxisAlignment: MainAxisAlignment.end,
              children: [
                TextButton(
                  onPressed: _saving
                      ? null
                      : () => setState(() {
                          _editing = false;
                          _error = null;
                        }),
                  child: const Text('取消'),
                ),
                const SizedBox(width: 8),
                FilledButton(
                  onPressed: _saving ? null : _save,
                  child: _saving
                      ? const SizedBox(
                          width: 18,
                          height: 18,
                          child: CircularProgressIndicator(strokeWidth: 2),
                        )
                      : const Text('保存'),
                ),
              ],
            ),
          ),
        ],
        const SizedBox(height: 24),
      ],
    );
  }

  Widget _readOnly(String title, String value) {
    return ListTile(title: Text(title), subtitle: Text(value));
  }
}
